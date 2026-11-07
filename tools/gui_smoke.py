#!/usr/bin/env python3
"""Run under xvfb-run on the Linux CI runner; never touches a user desktop."""
import os
import json
import math
import pathlib
import subprocess
import tempfile
import sys
import time

# Keep both successful and failed isolated-render evidence for Chinese text/layout.

def run_external(arguments, **options):
    options.setdefault("timeout", 10)
    return subprocess.run(arguments, **options)

root = pathlib.Path(__file__).resolve().parent.parent
with tempfile.TemporaryDirectory(prefix="peerglide-gui-ci-") as temporary:
    profile = pathlib.Path(temporary) / "explicit-profile"
    fallback = pathlib.Path(temporary) / "default-data"
    env = os.environ.copy()
    env.update(XDG_DATA_HOME=str(fallback), LIBGL_ALWAYS_SOFTWARE="1", WINIT_X11_SCALE_FACTOR="1")
    def capture(arguments, name):
        log = pathlib.Path(temporary) / f"{name}.log"
        with log.open("w") as output:
            geometry = pathlib.Path(temporary) / f"{name}-widget-geometry.json"
            if name in {"home", "gesture-lab"}:
                arguments = [*arguments, "--ui-layout-report", str(geometry)]
            gui = subprocess.Popen([str(root / "target/release/peerglide-gui"), "--data-dir", str(profile), *arguments], env=env, stdout=output, stderr=subprocess.STDOUT)
            try:
                for _ in range(100):
                    if gui.poll() is not None:
                        raise RuntimeError(log.read_text())
                    found = run_external(["xdotool", "search", "--onlyvisible", "--pid", str(gui.pid), "--name", "Linko"], capture_output=True, text=True)
                    if found.returncode == 0 and found.stdout.strip():
                        window = found.stdout.splitlines()[0]
                        break
                    time.sleep(0.1)
                else:
                    raise TimeoutError("GUI window did not appear")
                assert (profile / "gui-instance.lock").is_file(), "explicit data directory must own the GUI configuration"
                assert not (fallback / "peerglide").exists(), "explicit directory must not touch the default profile"
                time.sleep(1)
                run_external(["xdotool", "windowfocus", "--sync", window], check=True)
                if name in {"home", "gesture-lab"}:
                    for _ in range(30):
                        if geometry.exists():
                            layout = json.loads(geometry.read_text())
                            break
                        time.sleep(0.1)
                    else:
                        raise TimeoutError("real widget geometry was not reported")
                    assert layout["scope"] == "isolated_gui_widget_geometry"
                if name == "gesture-lab":
                    # Fixed-size, scale-1 virtual window. This is synthetic GUI
                    # input on our isolated X server, never a user desktop.
                    assert layout["version"] == 1
                    cx, cy, cw, ch = layout["canvas"]
                    sx, sy, sw, sh = layout["save"]
                    start_x, start_y = int(cx + 45), int(cy + ch / 2)
                    run_external(["xdotool", "mousemove", "--window", window, str(start_x), str(start_y)], check=True)
                    run_external(["xdotool", "mousedown", "3"], check=True)
                    for delta in (20, 45, 70, 100, 120):
                        run_external(["xdotool", "mousemove", "--window", window, str(start_x + delta), str(start_y)], check=True)
                        time.sleep(0.06)
                    run_external(["xdotool", "mouseup", "3"], check=True)
                    time.sleep(0.3)
                    folder = profile / "gesture-samples"
                    assert not folder.exists(), "recording must not autosave"
                    run_external(["xdotool", "mousemove", "--window", window, str(int(sx + sw / 2)), str(int(sy + sh / 2)), "click", "1"], check=True)
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
                    evaluation = run_external([
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
                run_external(["import", "-window", "root", str(path)], check=True)
                assert path.stat().st_size > 1000
                if name == "home":
                    # Scale-1 fresh unpaired workspace, owned Xvfb window only.
                    # Read these screenshots before claiming page/modal proof.
                    assert layout["version"] == 2
                    def click_control(key):
                        rect = layout["controls"][key]
                        assert (len(rect) == 4 and all(type(n) in {int, float} and math.isfinite(n) for n in rect)
                                and rect[0] >= 0 and rect[1] >= 0 and rect[2] > 0 and rect[3] > 0), key
                        x, y, width, height = rect
                        run_external(["xdotool", "mousemove", "--window", window,
                                      str(round(x + width / 2)), str(round(y + height / 2)), "click", "1"], check=True)
                    click_control("nav_settings")
                    time.sleep(0.4)
                    run_external(["import", "-window", "root", str(dest / "peerglide-settings.png")], check=True)
                    click_control("nav_layout")
                    time.sleep(0.3)
                    click_control("add_device")
                    time.sleep(0.4)
                    assert not (profile / "identity").exists(), "opening add must not create identity or click through"
                    run_external(["import", "-window", "root", str(dest / "peerglide-add-device.png")], check=True)
                run_external(["xdotool", "windowclose", window], check=True)
                try:
                    gui.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    gui.terminate()
                    gui.wait(timeout=3)
                print(f"PASS: {name} native GUI launched and rendered under isolated Xvfb")
            finally:
                primary_failure = sys.exc_info()[0] is not None
                cleanup_error = None
                commands = []
                if gui.poll() is None:
                    dest = root / "dist"
                    dest.mkdir(exist_ok=True)
                    commands.append(["import", "-window", "root", str(dest / f"peerglide-{name}-final.png")])
                commands.append(["xdotool", "mouseup", "3"])
                try:
                    for command in commands:
                        try:
                            run_external(command, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=True)
                        except (subprocess.SubprocessError, OSError) as error:
                            cleanup_error = error
                finally:
                    if gui.poll() is None:
                        gui.kill()
                        gui.wait(timeout=5)
                if cleanup_error is not None and not primary_failure:
                    raise RuntimeError("isolated GUI cleanup did not finish") from cleanup_error
    capture([], "home")
    capture(["--gesture-lab"], "gesture-lab")
