#!/usr/bin/env python3
"""Render the README animation, stills and posters.

The window in every image is the app's own markup and stylesheet loaded in
a headless browser, so the pictures follow app/ui instead of drifting from
it. Run from the repo root:

    python3 docs/media/src/render.py

Needs: playwright (with chromium), pillow, gifski, ffmpeg.
"""
import asyncio, functools, http.server, os, re, shutil, socketserver, subprocess, sys, tempfile, threading
from pathlib import Path
from PIL import Image
from playwright.async_api import async_playwright

ROOT = Path(__file__).resolve().parents[3]
SRC = Path(__file__).resolve().parent
OUT = SRC.parent
UI = ROOT / "app" / "ui"

FPS, LOOP = 16, 7.6                       # keep in step with T in scene.html
GIF_WIDTH = 900
POSTERS = {"P1": ("link-card", 1200, 630), "P2": ("read-the-answer", 1080, 1350),
           "P3": ("shrinks-itself", 1080, 1080), "P4": ("baked-for-47s", 1080, 1080),
           "P5": ("hero", 1600, 900), "P6": ("story", 1080, 1920),
           "P7": ("busy-then-done", 1080, 1350)}
STILL_T = {"idle": 1.55, "busy": 3.60, "done": 7.10}
VIEWPORTS = [("wide", 1440, 900, ["idle", "busy", "done"]),
             ("card", 1200, 630, ["idle"]),
             ("strip", 1360, 620, ["busy", "done"])]


def build_serve_dir(dst: Path):
    """Assemble the page and pull the stylesheet, fonts and marks from app/ui,
    so a change there shows up here without anything being copied by hand."""
    (dst / "fonts").mkdir(parents=True, exist_ok=True)
    (dst / "brand").mkdir(exist_ok=True)
    (dst / "plates").mkdir(exist_ok=True)
    (dst / "stills").mkdir(exist_ok=True)
    css = (UI / "src" / "style.css").read_text()
    # the @fontsource imports are bare specifiers Vite resolves; @font-face
    # in window.html covers them here
    (dst / "oterm.css").write_text(re.sub(r"^@import '@fontsource.*\n", "", css, flags=re.M))
    fdir = UI / "node_modules" / "@fontsource" / "ibm-plex-mono" / "files"
    for w in (400, 500, 600):
        shutil.copy(fdir / f"ibm-plex-mono-latin-{w}-normal.woff2", dst / "fonts")
    for b in ("claude", "gemini", "codex"):
        shutil.copy(UI / "src" / "brand" / f"{b}.svg", dst / "brand")
    for f in ("scene.html", "window.html", "window.js", "posters.html"):
        shutil.copy(SRC / f, dst)
    shutil.copy(SRC / "plates" / "video_day.jpg", dst / "plates")


def serve(directory: Path):
    h = functools.partial(http.server.SimpleHTTPRequestHandler, directory=str(directory))
    socketserver.TCPServer.allow_reuse_address = True
    s = socketserver.TCPServer(("127.0.0.1", 0), h)
    threading.Thread(target=s.serve_forever, daemon=True).start()
    return s, s.server_address[1]


async def open_scene(browser, port, w, h):
    p = await browser.new_page(viewport={"width": w, "height": h}, device_scale_factor=2)
    await p.goto(f"http://127.0.0.1:{port}/scene.html")
    await p.wait_for_function("window.sceneReady !== undefined")
    await p.evaluate("window.sceneReady")
    await p.wait_for_timeout(400)
    return p


async def main():
    work = Path(tempfile.mkdtemp(prefix="oterm-media-"))
    build_serve_dir(work)
    frames = work / "frames"; frames.mkdir()
    httpd, port = serve(work)
    async with async_playwright() as pw:
        browser = await pw.chromium.launch()

        # --- animation frames -------------------------------------------------
        page = await open_scene(browser, port, 1440, 900)
        n = int(round(LOOP * FPS))
        for i in range(n):
            await page.evaluate(f"window.setFrame({i / FPS})")
            await page.screenshot(path=str(frames / f"f{i:04d}.png"))
        print(f"captured {n} frames")

        # --- stills, one set per aspect the posters crop from ------------------
        for tag, w, h, states in VIEWPORTS:
            sp = page if (w, h) == (1440, 900) else await open_scene(browser, port, w, h)
            for st in states:
                await sp.evaluate(f"window.setFrame({STILL_T[st]});"
                                  "document.getElementById('vid').style.transform='scale(1.085)'")
                await sp.wait_for_timeout(150)
                await sp.screenshot(path=str(work / "stills" / f"{tag}_{st}.png"))
                if tag == "wide":
                    el = await sp.query_selector("#win")
                    await el.screenshot(path=str(work / "stills" / f"win_{st}.png"))
                if st == "idle":
                    await sp.evaluate("window.pose({t:1.0, mode:'bar', status:'idle',"
                                      "elapsed:'1m 40s / last 0s'});"
                                      "document.getElementById('vid').style.transform='scale(1.085)'")
                    await sp.wait_for_timeout(150)
                    await sp.screenshot(path=str(work / "stills" / f"{tag}_idlebar.png"))
            if sp is not page:
                await sp.close()
        print("captured stills")

        # --- posters ----------------------------------------------------------
        pp = await browser.new_page(viewport={"width": 1800, "height": 1000}, device_scale_factor=2)
        await pp.goto(f"http://127.0.0.1:{port}/posters.html")
        await pp.evaluate("window.postersReady")
        await pp.wait_for_timeout(700)
        (OUT / "posters").mkdir(exist_ok=True)
        for pid, (name, w, h) in POSTERS.items():
            el = await pp.query_selector("#" + pid)
            tmp = work / f"{pid}.png"
            await el.screenshot(path=str(tmp))
            Image.open(tmp).resize((w, h), Image.LANCZOS).save(OUT / "posters" / f"{name}-{w}x{h}.png", optimize=True)
        print(f"wrote {len(POSTERS)} posters")
        await browser.close()
    httpd.shutdown()

    # --- encode -------------------------------------------------------------
    subprocess.run(["gifski", "-o", str(OUT / "demo.gif"), "--fps", str(FPS),
                    "--width", str(GIF_WIDTH), "--quality", "88", "--no-sort",
                    *sorted(str(f) for f in frames.glob("f*.png"))], check=True,
                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    subprocess.run(["ffmpeg", "-y", "-framerate", str(FPS), "-i", str(frames / "f%04d.png"),
                    "-vf", "scale=1280:-2:flags=lanczos", "-c:v", "libx264",
                    "-pix_fmt", "yuv420p", "-crf", "20", "-movflags", "+faststart",
                    str(OUT / "demo.mp4")], check=True,
                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)

    st = work / "stills"
    for src, dst in [("wide_done", "expanded-over-video"), ("wide_busy", "collapsed-over-video"),
                     ("wide_idlebar", "idle-over-video")]:
        Image.open(st / f"{src}.png").convert("RGB").resize((1440, 900), Image.LANCZOS) \
             .save(OUT / f"{dst}.jpg", quality=92, optimize=True)
    Image.open(st / "win_busy.png").convert("RGB").save(OUT / "collapsed-bar.png", optimize=True)
    print("encoded demo.gif, demo.mp4 and the stills")
    shutil.rmtree(work)

asyncio.run(main())
