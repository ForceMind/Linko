#!/usr/bin/env python3
"""Run under xvfb-run on the Linux CI runner; never touches a user desktop."""
import os
import json
import pathlib
import subprocess
import tempfile
import time

root = pathlib.Path(__file__).resolve().parent.parent
with tempfile.TemporaryDirectory(prefix="peerglide-gui-ci-") as temporary:
    env = os.environ.copy()
    env.update(XDG_DATA_HOME=temporary, LIBGL_ALWAYS_SOFTWARE="1", WINIT_X11_SCALE_FACTOR="1")
    def capture(arguments, name):
        log = pathlib.Path(temporary) / f"{name}.log"
        with log.open("w") as output:
            gui = subprocess.Popen([str(root / "target/release/peerglide-gui"), *arguments], env=env, stdout=output, stderr=subprocess.STDOUT)
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
                    folder = pathlib.Path(temporary) / "peerglide" / "gesture-samples"
                    assert not folder.exists(), "recording must not autosave"
                    subprocess.run(["xdotool", "mousemove", "--window", window, "240", "440", "click", "1"], check=True)
                    for _ in range(30):
                        files = list(folder.glob("*.json"))
                        if files:
                            break
                        time.sleep(0.1)
                    assert len(files) == 1, "Save must create one labelled sample"
                    sample = json.loads(files[0].read_text())
                    assert sample["source"] == "egui_canvas" and sample["schema_version"] == 1
                    assert sample["expected_id"] is None, "default label must not follow recognition"
                    points = sample["samples"]
                    assert 2 <= len(points) <= 2048
                    assert points[-1]["x"] - points[0]["x"] > 60
                    print("PASS: synthetic canvas drawing stayed in memory, then explicit Save exported one labelled sample")
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
                subprocess.run(["xdotool", "mouseup", "3"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
                if gui.poll() is None:
                    gui.kill()
                    gui.wait()
    capture([], "home")
    capture(["--gesture-lab"], "gesture-lab")
