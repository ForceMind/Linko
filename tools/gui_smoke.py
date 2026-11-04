#!/usr/bin/env python3
"""Run under xvfb-run on the Linux CI runner; never touches a user desktop."""
import os
import json
import pathlib
import subprocess
import tempfile
import time

# Keep both successful and failed isolated-render evidence for Chinese text/layout.

root = pathlib.Path(__file__).resolve().parent.parent
with tempfile.TemporaryDirectory(prefix="peerglide-gui-ci-") as temporary:
    profile = pathlib.Path(temporary) / "explicit-profile"
    fallback = pathlib.Path(temporary) / "default-data"
    env = os.environ.copy()
    env.update(XDG_DATA_HOME=str(fallback), LIBGL_ALWAYS_SOFTWARE="1", WINIT_X11_SCALE_FACTOR="1")
    def capture(arguments, name):
        log = pathlib.Path(temporary) / f"{name}.log"
        with log.open("w") as output:
            gui = subprocess.Popen([str(root / "target/release/peerglide-gui"), "--data-dir", str(profile), *arguments], env=env, stdout=output, stderr=subprocess.STDOUT)
            try:
                for _ in range(100):
                    if gui.poll() is not None:
                        raise RuntimeError(log.read_text())
                    found = subprocess.run(["xdotool", "search", "--onlyvisible", "--pid", str(gui.pid), "--name", "PeerGlide"], capture_output=True, text=True)
                    if found.returncode == 0 and found.stdout.strip():
                        window = found.stdout.splitlines()[0]
                        break
                    time.sleep(0.1)
                else:
                    raise TimeoutError("GUI window did not appear")
                assert (profile / "gui-instance.lock").is_file(), "explicit data directory must own the GUI configuration"
                assert not (fallback / "peerglide").exists(), "explicit directory must not touch the default profile"
                time.sleep(1)
                subprocess.run(["xdotool", "windowfocus", "--sync", window], check=True)
                if name == "gesture-lab":
                    # Fixed-size, scale-1 virtual window. This is synthetic GUI
                    # input on our isolated X server, never a user desktop.
                    subprocess.run(["xdotool", "mousemove", "--window", window, "300", "200"], check=True)
                    subprocess.run(["xdotool", "mousedown", "3"], check=True)
                    for x in (320, 345, 370, 400, 420):
                        subprocess.run(["xdotool", "mousemove", "--window", window, str(x), "200"], check=True)
                        time.sleep(0.06)
                    subprocess.run(["xdotool", "mouseup", "3"], check=True)
                    time.sleep(0.3)
                    folder = profile / "gesture-samples"
                    assert not folder.exists(), "recording must not autosave"
                    subprocess.run(["xdotool", "mousemove", "--window", window, "240", "440", "click", "1"], check=True)
                    sample = None
                    for _ in range(30):
                        files = list(folder.glob("*.json"))
                        if len(files) == 1:
                            # Wait for asynchronous GUI I/O completion, not just
                            # directory entry creation. Persistent bad JSON fails.
                            try:
                                sample = json.loads(files[0].read_text())
                                break
                            except (FileNotFoundError, json.JSONDecodeError):
                                pass
                        time.sleep(0.1)
                    assert len(files) == 1, "Save must create one labelled sample"
                    assert sample is not None, "Save did not complete a valid JSON sample"
                    assert sample["source"] == "egui_canvas" and sample["schema_version"] == 1
                    assert sample["expected_id"] is None, "default label must not follow recognition"
                    points = sample["samples"]
                    assert 2 <= len(points) <= 2048
                    assert points[-1]["x"] - points[0]["x"] > 60
                    evaluation = subprocess.run([
                        "cargo", "run", "--quiet", "--locked", "-p", "peerglide-gestures",
                        "--example", "evaluate", "--", str(folder)
                    ], cwd=root, capture_output=True, text=True, timeout=60, check=True)
                    report = json.loads(evaluation.stdout)
                    assert report["samples"] == 1 and report["negative"] == 1
                    assert report["false_matches"] == 1 and not report["human_acceptance"]
                    print("PASS: synthetic canvas drawing stayed in memory, then explicit Save exported one labelled sample")
                    print("PASS: offline evaluator preserves the independent negative label and reports its false match")
                    time.sleep(0.2)
                dest = root / "dist"
                dest.mkdir(exist_ok=True)
                # Capture the virtual display; helper windows are not app windows.
                path = dest / f"peerglide-{name}.png"
                subprocess.run(["import", "-window", "root", str(path)], check=True)
                assert path.stat().st_size > 1000
                subprocess.run(["xdotool", "windowclose", window], check=True)
                try:
                    gui.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    gui.terminate()
                    gui.wait(timeout=3)
                print(f"PASS: {name} native GUI launched and rendered under isolated Xvfb")
            finally:
                if gui.poll() is None:
                    dest = root / "dist"
                    dest.mkdir(exist_ok=True)
                    subprocess.run(["import", "-window", "root", str(dest / f"peerglide-{name}-final.png")], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
                subprocess.run(["xdotool", "mouseup", "3"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
                if gui.poll() is None:
                    gui.kill()
                    gui.wait()
    capture([], "home")
    capture(["--gesture-lab"], "gesture-lab")
