# Icon source

`icon-source-1024.png` is what the icon set in `app/icons/` was generated
from:

```sh
npm run tauri -- icon design/icon-source-1024.png
```

Keeping the source here means the set can be rebuilt rather than being a
pile of PNGs nobody can regenerate. Delete the `android/` and `ios/`
directories it also writes; this is a desktop app.

The artwork is the app's own window: a dark rounded square, a white chrome
bar carrying the status dot, and a lowercase `o` beside an amber block
cursor. The status dot on the bar is the point of it, so a busy icon in
the Dock is a real signal.
