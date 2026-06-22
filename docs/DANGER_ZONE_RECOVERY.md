# CBZ Viewer Danger Zone Recovery

Danger Zone lets you raise cache and worker limits beyond the normal safe range.
If those values prevent the app from starting, restore the safe defaults below.

## Settings file

`%LOCALAPPDATA%\cbz-viewer\settings.json`

On a typical Windows system, this is:

`C:\Users\<UserName>\AppData\Local\cbz-viewer\settings.json`

## If CBZ Viewer no longer starts

1. Quit CBZ Viewer.
2. Open `settings.json` in a text editor.
3. Lower the L1 cache, L2 cache, and background worker values.
4. Save the file and restart the app.

Safe values to restore:

L1 VRAM cache: 256 MiB
L2 RGBA cache: 256 MiB
Background workers: 2
Danger Zone: disabled

The values above are conservative recovery values.
Deleting or renaming `settings.json` recreates hardware-derived defaults.

## If you cannot edit the file

Delete or rename `settings.json` and start the app again.
The default settings will be recreated on the next launch.

Changing or deleting only `settings.json` does not remove per-book display settings, history, thumbnails, or favorites.
