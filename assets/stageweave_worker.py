"""StageWeave separation worker.

Run by the StageWeave app inside its private Python environment. Speaks a tiny
JSON-lines protocol on stdout; everything else (library logs, warnings) goes
to stderr.

    python stageweave_worker.py env-info
    python stageweave_worker.py status   --models-dir DIR [--remote] MODEL...
    python stageweave_worker.py download --models-dir DIR MODEL
    python stageweave_worker.py delete   --models-dir DIR MODEL
    python stageweave_worker.py separate --engine demucs|uvr --model ID \
        --input FILE --out DIR [--device auto|cpu|cuda|mps] [--models-dir DIR]

MODEL is written as `engine:id`, e.g. `demucs:htdemucs_6s` or
`uvr:UVR_MDXNET_KARA_2.onnx`.

Messages:
    {"t": "stage", "text": "..."}
    {"t": "progress", "v": 0.0-1.0}
    {"t": "download", "done": bytes, "total": bytes|null, "file": "name"}
    {"t": "model", "key": "engine:id", "downloaded": bool, "on_disk": bytes, "total": bytes|null}
    {"t": "stem", "kind": "vocals", "path": "/abs/path.wav"}
    {"t": "env", ...}
    {"t": "done"}
    {"t": "error", "msg": "..."}

Engines:
    demucs: https://github.com/adefossez/demucs (MIT). Weights come from the
            Hugging Face hub (`adefossez/HTDemucs*`).
    uvr:    Ultimate Vocal Remover models (https://github.com/Anjok07/ultimatevocalremovergui)
            run headless through python-audio-separator (MIT).
"""

import argparse
import json
import os
import shutil
import sys
import tempfile
import time
import traceback
from pathlib import Path

# Plain HTTP downloads write `.incomplete` files we can resume and measure.
os.environ.setdefault("HF_HUB_DISABLE_XET", "1")

# Keep a private handle on the real stdout for protocol messages, then point
# fd 1 at stderr so stray prints from libraries can't corrupt the protocol.
_PROTO = os.fdopen(os.dup(1), "w", buffering=1, encoding="utf-8")
os.dup2(2, 1)
sys.stdout = sys.stderr

def watch_parent():
    """Exit when the app goes away. The app keeps our stdin open; when it
    exits (or is killed) the pipe closes and the read returns."""
    import threading

    def run():
        # Raw reads on fd 0: a thread blocked inside sys.stdin's buffered
        # reader makes CPython abort at shutdown ("_enter_buffered_busy").
        try:
            while os.read(0, 4096):
                pass
        except OSError:
            pass
        os._exit(1)

    threading.Thread(target=run, daemon=True).start()


if os.environ.get("STAGEWEAVE_WATCH_PARENT") == "1":
    watch_parent()

KNOWN_STEMS = ["vocals", "drums", "bass", "guitar", "piano", "other", "instrumental"]


def emit(**msg):
    _PROTO.write(json.dumps(msg) + "\n")
    _PROTO.flush()


def stage(text):
    emit(t="stage", text=text)


class Progress:
    """Monotonic progress so restarting inner loops never move the bar back."""

    def __init__(self, lo=0.0, hi=1.0):
        self.lo, self.hi, self.last = lo, hi, -1.0

    def __call__(self, frac):
        v = self.lo + (self.hi - self.lo) * max(0.0, min(1.0, frac))
        if v >= self.last + 0.002:
            self.last = v
            emit(t="progress", v=round(v, 4))


class DownloadReporter:
    """Throttled byte-progress messages across several files."""

    def __init__(self, total=None):
        self.total = total
        self.base = 0  # bytes of files already finished
        self.last_emit = 0.0

    def report(self, current, name, force=False):
        now = time.monotonic()
        if force or now - self.last_emit > 0.2:
            self.last_emit = now
            emit(t="download", done=self.base + current, total=self.total, file=name)

    def finish_file(self, size):
        self.base += size


def pick_device(requested):
    import torch

    if requested and requested != "auto":
        return requested
    if torch.cuda.is_available():
        return "cuda"
    if getattr(torch.backends, "mps", None) and torch.backends.mps.is_available():
        return "mps"
    return "cpu"


def version_of(pkg):
    from importlib.metadata import PackageNotFoundError, version

    try:
        return version(pkg)
    except PackageNotFoundError:
        return None


def parse_model(key):
    engine, _, model_id = key.partition(":")
    if engine not in ("demucs", "uvr") or not model_id:
        raise SystemExit(f"bad model key {key!r}, expected engine:id")
    return engine, model_id


def cmd_env_info(_args):
    import platform

    info = {
        "python": platform.python_version(),
        "torch": version_of("torch"),
        "demucs": version_of("demucs"),
        "audio_separator": version_of("audio-separator"),
        "device": None,
    }
    try:
        info["device"] = pick_device("auto")
    except Exception as e:  # torch missing or broken
        info["error"] = str(e)
    emit(t="env", **info)
    emit(t="done")


# --------------------------------------------------------------------------- tqdm hook

_TQDM_LISTENERS = []


def install_tqdm_hook():
    """Route every tqdm bar's updates to the active listeners.

    Must run before huggingface_hub / audio_separator are imported, because
    they subclass or import tqdm at import time.
    """
    import tqdm
    import tqdm.auto

    if getattr(tqdm.tqdm, "_stageweave", False):
        return
    base = tqdm.auto.tqdm

    class ReportingTqdm(base):
        _stageweave = True

        def update(self, n=1):
            r = super().update(n)
            for listener in list(_TQDM_LISTENERS):
                try:
                    listener(self)
                except Exception:
                    pass
            return r

    tqdm.tqdm = ReportingTqdm
    tqdm.auto.tqdm = ReportingTqdm
    try:
        import tqdm.std

        tqdm.std.tqdm = ReportingTqdm
    except ImportError:
        pass


def is_byte_bar(bar):
    return getattr(bar, "unit", "") in ("B", "iB")


# --------------------------------------------------------------------------- demucs models


def demucs_repo(name):
    """Same mapping as demucs.hf.hf_repo_name."""
    if name == "htdemucs":
        repo = "HTDemucs"
    elif name.startswith("htdemucs_"):
        repo = "HTDemucs-" + name[len("htdemucs_") :]
    else:
        repo = "Demucs-" + name
    return "adefossez/" + repo


def demucs_files(name, allow_network):
    """Files the model needs: its bag yaml plus one safetensors per sub-model."""
    import yaml
    from huggingface_hub import hf_hub_download, try_to_load_from_cache

    repo = demucs_repo(name)
    yaml_name = f"{name}.yaml"
    path = try_to_load_from_cache(repo, yaml_name)
    if not isinstance(path, str):
        if not allow_network:
            return repo, None
        path = hf_hub_download(repo, yaml_name)
    with open(path) as f:
        sigs = yaml.safe_load(f).get("models", [])
    return repo, [yaml_name] + [f"{s}.safetensors" for s in sigs]


def demucs_status(name, remote):
    from huggingface_hub import hf_hub_download, try_to_load_from_cache

    repo, files = demucs_files(name, allow_network=remote)
    if files is None:
        return False, 0, None
    on_disk, have_all, total = 0, True, 0
    for fname in files:
        p = try_to_load_from_cache(repo, fname)
        if isinstance(p, str) and os.path.exists(p):
            on_disk += os.path.getsize(p)
        else:
            have_all = False
        if remote:
            info = hf_hub_download(repo, fname, dry_run=True)
            total += info.file_size or 0
    return have_all, on_disk, (total if remote else (on_disk if have_all else None))


def demucs_download(name):
    from huggingface_hub import hf_hub_download

    install_tqdm_hook()
    stage("Looking up model files")
    repo, files = demucs_files(name, allow_network=True)
    infos = [hf_hub_download(repo, f, dry_run=True) for f in files]
    reporter = DownloadReporter(total=sum(i.file_size or 0 for i in infos))
    for fname, info in zip(files, infos):
        size = info.file_size or 0
        if info.is_cached:
            reporter.finish_file(size)
            continue
        stage(f"Downloading {fname}")

        def on_bar(bar, fname=fname):
            if is_byte_bar(bar):
                reporter.report(bar.n, fname)

        _TQDM_LISTENERS.append(on_bar)
        try:
            hf_hub_download(repo, fname)
        finally:
            _TQDM_LISTENERS.remove(on_bar)
        reporter.finish_file(size)
        reporter.report(0, fname, force=True)


def demucs_delete(name):
    from huggingface_hub import constants

    repo_dir = Path(constants.HF_HUB_CACHE) / ("models--" + demucs_repo(name).replace("/", "--"))
    shutil.rmtree(repo_dir, ignore_errors=True)


# --------------------------------------------------------------------------- uvr models

UVR_PUBLIC = "https://github.com/TRvlvr/model_repo/releases/download/all_public_uvr_models"
UVR_VIP = "https://github.com/Anjok0109/ai_magic/releases/download/v5"
AS_CONFIGS = "https://github.com/nomadkaraoke/python-audio-separator/releases/download/model-configs"


def uvr_separator(models_dir, output_dir=None):
    install_tqdm_hook()
    import logging

    from audio_separator.separator import Separator

    patch_uvr_downloader(Separator)
    return Separator(
        output_dir=output_dir or tempfile.gettempdir(),
        model_file_dir=models_dir,
        output_format="WAV",
        log_level=logging.WARNING,
        info_only=output_dir is None,
    )


def uvr_files(sep, model_filename):
    """[(local filename, [candidate urls])] mirroring Separator.download_model_files."""
    for model_type, models in sep.list_supported_model_files().items():
        for friendly, info in models.items():
            if info["filename"] != model_filename and model_filename not in info["download_files"]:
                continue
            prefix = UVR_VIP if "VIP" in friendly else UVR_PUBLIC
            out = []
            for f in info["download_files"]:
                if f.startswith("http"):
                    out.append((f.split("/")[-1], [f]))
                elif model_type == "MDXC" and f.endswith(".yaml"):
                    out.append((f, [f"{prefix}/mdx_model_data/mdx_c_configs/{f}", f"{AS_CONFIGS}/{f}"]))
                else:
                    out.append((f, [f"{prefix}/{f}", f"{AS_CONFIGS}/{f}"]))
            return out
    raise ValueError(f"Unknown UVR model {model_filename}")


def remote_size(urls):
    import requests

    for url in urls:
        try:
            r = requests.head(url, allow_redirects=True, timeout=30)
            if r.status_code == 200:
                return int(r.headers.get("content-length", 0)) or None
        except requests.RequestException:
            continue
    return None


def download_atomic(url, dest, reporter=None, name=None):
    """Resumable download to `dest.part`, renamed into place only when complete."""
    import requests

    dest = Path(dest)
    if dest.is_file():
        return
    part = dest.with_name(dest.name + ".part")
    have = part.stat().st_size if part.exists() else 0
    headers = {"Range": f"bytes={have}-"} if have else {}
    with requests.get(url, stream=True, timeout=60, headers=headers) as r:
        if r.status_code == 416:  # already complete
            part.replace(dest)
            return
        if r.status_code not in (200, 206):
            raise RuntimeError(f"Failed to download file from {url}, response code: {r.status_code}")
        if r.status_code == 200:
            have = 0  # server ignored the range; start over
        mode = "ab" if have else "wb"
        with open(part, mode) as f:
            done = have
            for chunk in r.iter_content(chunk_size=1 << 16):
                f.write(chunk)
                done += len(chunk)
                if reporter:
                    reporter.report(done, name or dest.name)
    part.replace(dest)


def patch_uvr_downloader(separator_cls):
    """audio-separator writes straight to the final path, so an interrupted
    download looks complete next time. Swap in the atomic downloader."""

    def download_file_if_not_exists(self, url, output_path):
        download_atomic(url, output_path)

    separator_cls.download_file_if_not_exists = download_file_if_not_exists


def uvr_status(models_dir, model_id, remote):
    sep = uvr_separator(models_dir)
    files = uvr_files(sep, model_id)
    on_disk, have_all, total = 0, True, 0
    for fname, urls in files:
        p = Path(models_dir) / fname
        if p.is_file():
            on_disk += p.stat().st_size
        else:
            have_all = False
        if remote:
            total += remote_size(urls) or 0
    return have_all, on_disk, (total if remote else (on_disk if have_all else None))


def uvr_download(models_dir, model_id):
    Path(models_dir).mkdir(parents=True, exist_ok=True)
    stage("Looking up model files")
    sep = uvr_separator(models_dir)
    files = uvr_files(sep, model_id)
    sizes = [remote_size(urls) for _, urls in files]
    reporter = DownloadReporter(total=sum(s or 0 for s in sizes) or None)
    for (fname, urls), size in zip(files, sizes):
        dest = Path(models_dir) / fname
        if dest.is_file():
            reporter.finish_file(dest.stat().st_size)
            continue
        stage(f"Downloading {fname}")
        last_err = None
        for url in urls:
            try:
                download_atomic(url, dest, reporter, fname)
                last_err = None
                break
            except RuntimeError as e:
                last_err = e
        if last_err:
            raise last_err
        reporter.finish_file(dest.stat().st_size)
        reporter.report(0, fname, force=True)


def uvr_delete(models_dir, model_id):
    sep = uvr_separator(models_dir)
    for fname, _ in uvr_files(sep, model_id):
        for p in (Path(models_dir) / fname, Path(models_dir) / (fname + ".part")):
            if p.exists():
                p.unlink()


# --------------------------------------------------------------------------- model commands


def cmd_status(args):
    for key in args.models:
        engine, model_id = parse_model(key)
        try:
            if engine == "demucs":
                downloaded, on_disk, total = demucs_status(model_id, args.remote)
            else:
                downloaded, on_disk, total = uvr_status(args.models_dir, model_id, args.remote)
            emit(t="model", key=key, downloaded=downloaded, on_disk=on_disk, total=total)
        except Exception as e:
            traceback.print_exc()
            emit(t="model", key=key, downloaded=False, on_disk=0, total=None, error=str(e))
    emit(t="done")


def cmd_download(args):
    engine, model_id = parse_model(args.model)
    if engine == "demucs":
        demucs_download(model_id)
    else:
        uvr_download(args.models_dir, model_id)
    emit(t="done")


def cmd_delete(args):
    engine, model_id = parse_model(args.model)
    if engine == "demucs":
        demucs_delete(model_id)
    else:
        uvr_delete(args.models_dir, model_id)
    emit(t="done")


# --------------------------------------------------------------------------- separation


def separate_demucs(args, out):
    install_tqdm_hook()
    from demucs.api import Separator, save_audio

    device = pick_device(args.device)
    stage(f"Loading {args.model} on {device.upper()}")
    progress = Progress(0.0, 0.97)

    def on_progress(d):
        if d.get("state") != "end":
            return
        models = max(1, d.get("models", 1))
        length = max(1, d.get("audio_length", 1))
        frac = (d.get("model_idx_in_bag", 0) + d.get("segment_offset", 0) / length) / models
        progress(frac)

    separator = Separator(model=args.model, device=device, callback=on_progress, progress=False)
    stage("Separating")
    _, stems = separator.separate_audio_file(Path(args.input))

    stage("Writing stems")
    for name, wav in stems.items():
        kind = name if name in KNOWN_STEMS else "other"
        path = out / f"{kind}.wav"
        save_audio(wav, path, samplerate=separator.samplerate)
        emit(t="stem", kind=kind, path=str(path.resolve()))


def stem_kind_from_name(filename):
    """Map audio-separator outputs like 'song_(Vocals)_model.wav' to a stem kind."""
    lower = filename.lower()
    tag = lower[lower.find("(") + 1 : lower.find(")")] if "(" in lower else lower
    if "instrumental" in tag or "no vocals" in tag or "no_vocals" in tag:
        return "instrumental"
    for kind in KNOWN_STEMS:
        if kind in tag:
            return kind
    return "other"


def separate_uvr(args, out):
    progress = Progress(0.0, 0.97)
    models_dir = args.models_dir or str(out.parent / "models")
    raw = out / "_raw"
    raw.mkdir(parents=True, exist_ok=True)
    separator = uvr_separator(models_dir, output_dir=str(raw))

    def on_bar(bar):
        if not is_byte_bar(bar) and bar.total:
            progress(bar.n / bar.total)

    _TQDM_LISTENERS.append(on_bar)

    stage(f"Loading {args.model}")
    separator.load_model(model_filename=args.model)
    stage("Separating")
    files = separator.separate(args.input)

    stage("Writing stems")
    used = set()
    for f in files:
        src = Path(f)
        if not src.is_absolute():
            src = raw / src
        kind = stem_kind_from_name(src.name)
        if kind in used:  # never overwrite a stem with a second one of the same kind
            kind = "other"
        used.add(kind)
        dst = out / f"{kind}.wav"
        os.replace(src, dst)
        emit(t="stem", kind=kind, path=str(dst.resolve()))
    shutil.rmtree(raw, ignore_errors=True)


def cmd_separate(args):
    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    if args.engine == "demucs":
        separate_demucs(args, out)
    elif args.engine == "uvr":
        separate_uvr(args, out)
    else:
        raise SystemExit(f"unknown engine {args.engine}")
    emit(t="progress", v=1.0)
    emit(t="done")


def main():
    parser = argparse.ArgumentParser(prog="stageweave_worker")
    sub = parser.add_subparsers(dest="cmd", required=True)
    sub.add_parser("env-info")

    status = sub.add_parser("status")
    status.add_argument("--models-dir", required=True)
    status.add_argument("--remote", action="store_true", help="also fetch download sizes")
    status.add_argument("models", nargs="+")

    for name in ("download", "delete"):
        p = sub.add_parser(name)
        p.add_argument("--models-dir", required=True)
        p.add_argument("model")

    sep = sub.add_parser("separate")
    sep.add_argument("--engine", required=True, choices=["demucs", "uvr"])
    sep.add_argument("--model", required=True)
    sep.add_argument("--input", required=True)
    sep.add_argument("--out", required=True)
    sep.add_argument("--device", default="auto")
    sep.add_argument("--models-dir", default=None)
    args = parser.parse_args()

    commands = {
        "env-info": cmd_env_info,
        "status": cmd_status,
        "download": cmd_download,
        "delete": cmd_delete,
        "separate": cmd_separate,
    }
    code = 0
    try:
        commands[args.cmd](args)
    except KeyboardInterrupt:
        emit(t="error", msg="Cancelled")
        code = 130
    except Exception as e:
        traceback.print_exc()
        emit(t="error", msg=f"{type(e).__name__}: {e}")
        code = 1
    # Skip interpreter teardown: native libraries (torch, onnxruntime) keep
    # background threads that can crash Python while it finalizes.
    _PROTO.flush()
    sys.stderr.flush()
    os._exit(code)


if __name__ == "__main__":
    main()
