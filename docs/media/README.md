# Media

Screenshots, the demo animation and the poster set.

| File | Used for |
|---|---|
| `demo.gif` | The animation at the top of the README |
| `demo.mp4` | The same footage, for release notes and anywhere a video tag works |
| `collapsed-over-video.jpg` | The status bar over a full-screen video |
| `expanded-over-video.jpg` | The full window over a full-screen video |
| `idle-over-video.jpg` | An idle session sitting in the corner |
| `collapsed-bar.png` | The status bar on its own, no background |

`posters/` holds the launch images, each named for the line it carries and
the size it was cut to. `link-card-1200x630.png` is the one to set under
Settings, General, Social preview.

## Regenerating

Everything here comes out of one script:

```sh
python3 docs/media/src/render.py
```

It needs `playwright` (with chromium installed), `pillow`, `gifski` and
`ffmpeg`, and it takes about a minute.

The window in every frame is `app/ui/src/style.css` and the same markup as
`app/ui/index.html`, loaded in a headless browser. The script reads the
stylesheet, the fonts and the agent marks straight out of `app/ui`, so
changing the interface and re-running is enough to bring these back in
line. Timings live in `src/scene.html`; the poster copy and layout live in
`src/posters.html`.

The video behind the window is one of my own screen captures, blurred and
darkened so it reads as playing footage without carrying anything
identifiable from the original.
