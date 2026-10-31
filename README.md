# Screen Mirror

A small Windows tool that mirrors one monitor onto another, written in Rust.

Windows' built-in "Duplicate these displays" mode forces both screens to share a
resolution, which gets in the way if you use custom resolutions (for example a
stretched 4:3 resolution for Counter-Strike) on your main monitor. Screen Mirror
leaves the source monitor alone and simply shows a live copy of it, scaled to fit,
on the monitor of your choice (a TV, a second screen, a capture device, etc.).

## Features

- Mirrors any monitor to any other monitor, each with its own resolution
- Keeps working when the source resolution changes while it is running
- Real monitor names (read from the display's EDID) instead of "Display 2"
- Visual layout panel that draws your monitors in their real arrangement:
  left click a monitor to pick the one to mirror
- The **Open monitor** dialog asks where to show the copy and whether it should
  start fullscreen, remembering both choices for the next time. The source
  monitor itself can be picked too, which shows a windowed copy of that screen
- Optional VSync
- The mirror opens as a floating window on the chosen monitor; press
  **F11** to make it fullscreen and **Esc** to go back
- Starting the mirror never takes focus, so it won't pull you out of a game
- Minimizing the window sends it to the system tray; the mirror keeps running
- GPU-only pipeline (no CPU copies), no console window, a single small executable

## How it works

1. The source monitor is captured with the
   [Windows Graphics Capture](https://learn.microsoft.com/en-us/windows/uwp/audio-video-camera/screen-capture)
   API, which delivers frames as Direct3D 11 textures.
2. Each frame is copied on the GPU into the back buffer of a flip-model DXGI
   swap chain attached to a window on the destination monitor.
3. The swap chain uses `DXGI_SCALING_STRETCH`, so the desktop compositor scales
   the image to the destination window, both in the small floating window and in
   fullscreen (F11).

It talks to Windows through the [`windows`](https://crates.io/crates/windows)
crate and uses plain Win32 controls for the interface, so there are no other
dependencies.

## Requirements

- Windows 10 version 1903 or later (Windows 11 recommended, see notes below)
- A GPU with Direct3D 11 support
- [Rust](https://rustup.rs/) with the MSVC toolchain (`x86_64-pc-windows-msvc`)
  to build from source

## Build and run

```powershell
git clone https://github.com/Ryu2O77/ScreenMirror.git
cd ScreenMirror
cargo run --release
```

The optimized executable ends up in `target\release\screen-mirror.exe`.

## Usage

1. Start the program. The panel at the top shows your monitors as they are
   arranged in Windows.
2. Select the monitor you want to copy, either with the drop-down list or with a
   **left click** on the panel (it turns blue and is tagged `[SOURCE]`).
3. Optionally tick **VSync** to avoid tearing at the cost of a little latency.
4. Press **Open monitor...**. A small dialog asks **Show on** (which monitor
   displays the copy; the source monitor is listed as `(same monitor)` if you
   want a copy on the same screen, in which case **Open fullscreen** is disabled)
   and **Open fullscreen**. The `[MIRROR]` tag in the panel follows the window,
   even while you drag it to another monitor.
5. Press **Open**. A floating window appears on the chosen monitor showing a live
   copy of the source. Press **F11** on it to go fullscreen and **Esc** to return
   to the floating window.
6. Press **Stop** to close the mirror. Use **Refresh monitors** if you connect a
   display or change a resolution while the program is open.
7. **Minimizing** the window hides it to the system tray and the mirror keeps
   running. Click the tray icon to bring the window back, or right click it for
   **Open** and **Exit**. On Windows 11 the icon may live in the hidden-icons
   overflow until you pin it.

While the mirror is running, the selection is locked; stop it first to change it.

## Notes and limitations

- **Aspect ratio:** the image is stretched to fill the destination monitor. If
  the source uses a stretched resolution, the destination will show it stretched
  as well. Letterboxing is not implemented yet.
- **Yellow border:** on Windows 10 the capture API draws a yellow border around
  the captured monitor. Windows 11 allows it to be turned off, and the program
  requests that automatically.
- **Exclusive fullscreen:** games running in true exclusive fullscreen may show
  up black in the capture. Borderless or windowed fullscreen modes work.
- **Keyboard:** **F11** and **Esc** act on the mirror window, so click it once if
  it does not have focus. Start it with the mouse in the control window and the
  mirror will not steal focus from your game.
- **Tray:** with the window minimized the app only lives in the system tray.
  If the icon is not visible, check the hidden-icons overflow next to the clock.
- **Performance:** capturing costs some GPU time. If you are sensitive to it,
  compare your frame rate with and without the program running.
- **Latency:** VSync is off by default for the lowest latency; the copy is
  performed on the GPU, but a frame or so of delay is normal for any
  capture-based approach.
- **Windows 10 look:** the interface uses classic Win32 controls without a
  visual-styles manifest, so it looks like an older Windows app.

## Project layout

```
Cargo.toml         Package manifest and the `windows` crate features
build.rs           Embeds the application icon and version info into the .exe
assets/icon.ico    Application icon used by build.rs
src/main.rs        Capture engine, window procedures and GUI
```

## Contributing

Issues and pull requests are welcome.

## License

Released under the [MIT License](LICENSE).
