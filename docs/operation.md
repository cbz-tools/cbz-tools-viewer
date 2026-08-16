[日本語](operation.ja.md)

# CBZ Viewer Operation Guide

## GUI

### Getting started

When the app starts, the Library screen appears.

Enter the folder you want to browse in the path field at the top of the Library screen.

You can specify a folder by:

* Typing a path
* Choosing a folder from the dialog
* Dragging and dropping a folder

The books inside the selected folder are listed.
Folders with images directly under them are also shown as books.

Double-click a book to open the Viewer.

### Settings

The Settings window has six tabs: General, Library, Viewer, Performance, External Tools, and Web Search.

General > App > Language switches the UI language between English and Japanese.

Language changes take effect immediately, and no restart is required.

### Library settings

The Library tab is split into List, Card Display, and Image Folder.

* List controls card size and wheel speed.
* Card Display controls HUD text and the appearance of card HUD and selected-card highlighting.
* Image Folder controls whether image folders open as books.

Changes are applied immediately.

### Viewer settings

* Display controls the global quality setting used by the Viewer.
* Reading controls the default reading direction and whether the Viewer resumes from the last reading position.
* Open rebuilt archive in a new Viewer is off by default.
  When off, the current Viewer only moves to the next book.
  When on, the rebuilt archive also opens in another Viewer window.

### External Tools settings

Open Settings > External Tools; all 3 tool slots are always shown and configured in place. Each slot has Name, Shortcut, Start mode (Background or Normal), Executable, and Arguments. Set Executable to the program to start. In Arguments, `{path}` is replaced with the full path of the target book. Edit or blank the fields in a slot to configure it or leave it unused.

In the Library, right-click a book (or selected books) and choose External Tools > a configured tool. In the Viewer, use the External Tools toolbar or the configured shortcut. A tool must have a non-empty Name and Executable to appear in these launch surfaces. The app starts the configured executable with the target path; it does not provide or install the external tool.

### Web Search settings

The Web Search tab selects one browser (Chrome, Edge, or Firefox), one open mode (Tab or New Window), and up to five search destinations. Each destination has Display and Link fields; both must be non-empty, and Link must contain `%s`, which is replaced with the URL-encoded selected token. Incomplete destinations are not shown in the token menu. If there are no valid destinations, the Filter, Copy, and Clear Filter items remain available.

The configured destinations are available from the shared filename-token menu in both Library and Viewer. Select a filename token, then choose `Search ... with ...` for the desired destination. The same menu is available in Library, Library-connected Viewer, Standalone Viewer, and Detached Viewer; Web Search does not require IPC. Token selection keeps the menu open so Filter, Copy, or Web Search can be chosen without reopening it. Choosing a destination builds a URL and starts the selected external browser with the selected Tab/New Window mode, then closes the menu. The app only starts that external browser process; it does not make HTTP requests or display search results itself. Only `http://` and `https://` links are executed. If the selected browser is unavailable, the app safely leaves the search unopened.

### Performance

* L1 VRAM Cache: Stores GPU textures for upcoming and recently viewed pages.
* L2 RAM Cache: Stores decoded RGBA images in system memory. Adjacent-book preloading uses 5% of this setting for each of the next and previous books by default.
* Background Workers: Controls background decoding parallelism.
* Danger Zone: Allows manual values beyond the normal hardware-based limits, including Adjacent Book Preload RAM (5–30% per adjacent book). The two-page preload guarantee is separate, and changes take effect the next time the Viewer starts.

---

## Library screen

### Basic actions

| Action | Result |
| --- | --- |
| Double-click | Open book |
| Enter | Open book |
| Alt+Enter | Show properties for the selected item |
| Delete | Delete selected book |
| F2 | Rename |
| Ctrl+A | Select all |
| Ctrl+C | Copy path |
| Ctrl+Wheel Up | Increase thumbnail display size by one step |
| Ctrl+Wheel Down | Decrease thumbnail display size by one step |

### Thumbnail display size

The thumbnail display width can be changed from 120 to 660 px in 20 px steps. The default is 200 px.

* Change it from the card-size setting under Library > List.
* On the Library grid, Ctrl+Wheel Up / Down increases or decreases the size by 20 px per wheel notch.
* Values changed with Ctrl+Wheel are saved to settings and restored on the next launch.
* Normal wheel input continues to scroll the Library vertically.
* Thumbnail width itself is not stretched to fill the available row width. Extra horizontal space is distributed evenly across the left edge, right edge, and gaps between columns.
* The existing minimum inter-column gap is always preserved. A partially filled final row keeps the same column positions as the rows above it.

### Video thumbnail preview

For video files, hovering over the thumbnail starts a preview after a short delay.

* Hover outside the filename HUD area to play an automatic preview from 10% to 90% of the video.
* Hover over the filename HUD area and move left or right to scrub through the same 10% to 90% range.
* When the card HUD is hidden, the same bottom area is still used for scrubbing.
* Moving between the preview area and scrub area switches modes immediately.
* Moving the pointer away restores the normal thumbnail.

### Animated WebP thumbnail preview

After a short hover delay, an Archive or image-file thumbnail plays in place when its thumbnail source is an animated WebP. For archives, only the first image is played.

* Hover outside the filename HUD area to play the animated WebP automatically.
* Hover over the filename HUD area and move left or right to immediately show the still frame at that point in the animation.
* A one-page book whose page is an animated WebP uses the same time scrub. Multi-page books use page scrubbing instead.
* When the card HUD is hidden, the same bottom area is still used for scrubbing.
* Moving between the preview area and scrub area switches modes immediately.
* Moving the pointer away restores the normal thumbnail.

### Static-image thumbnail scrubbing

For multi-page image books with an available Page Map, Static Scrub can be used on Archive, FolderBook, EPUB image book, and RAR/CBR entries. Multi-page books use the page axis even when a page is an animated WebP.

* Move left or right in the filename HUD area to scrub through all pages.
* The page at the pointed position is shown in the thumbnail.
* There is no automatic preview.
* When the card HUD is hidden, the same bottom area is still used for scrubbing.
* Moving the pointer outside the scrub area restores the normal thumbnail.
* Static Scrub is not used for image books without a Page Map.

### Search

Use the search box to filter by book title.

### Favorites

You can show only books that are marked as favorites.

### Reading status

You can filter books by Unread / Reading / Read. Counts are shown.

Reading status updates automatically from Viewer progress. Closing a book before the end marks it as Reading, and closing it after showing the last page marks it as Read.

### Extensions

You can filter Library items by file extension. Counts are shown, and the filter can be combined with the search box.

### Groups

You can organize books into groups.

See [Book group settings](book-groups.md) for details.

### Right-click menu

| Item | Result |
| --- | --- |
| Open | Open book |
| Open in Explorer | Show in Explorer |
| Add to Favorites / Remove from Favorites | Toggle favorite state |
| Set Group | Assign a group |
| Clear Book Settings | Reset book-specific display settings and reading status |
| Rename | Rename |
| Copy | Copy path |
| Delete | Delete |
| Properties | Show details for the selected item |
| External Tools | Run external tools |

When a FolderBook is selected, `Move to folder` is also shown.

The selected book file or folder is deleted, and related thumbnails, Page Map, favorites, and groups are removed.

Some items are hidden or disabled when multiple items are selected.

For a single Archive, the shared filename-token menu provides Filter by token, Clear Filter, Copy token, and the configured Web Search destinations. Token selection keeps the menu open; choosing Filter, Clear Filter, Copy, or Web Search executes it and closes the menu. Library Clear Filter clears the keyword filter and marks the existing filter for refresh.

---

## Viewer screen

The Viewer uses the same filename-token menu and token selection as the Library. Filter by token is sent through the existing Viewer-to-Library filter IPC only when the Viewer has Library/Snapshot IPC. Clear Filter is always active: with that IPC it sends the symmetric minimal clear request to Library; Standalone, Detached, and CLI Viewers safely no-op. Copy and Web Search do not require IPC and use the local clipboard or external-browser launch described above.

### Page navigation

| Action | Result |
| --- | --- |
| ← / → / A / D | Page navigation |
| PageDown | Next page |
| PageUp | Previous page |
| Wheel down | Next page |
| Wheel up | Previous page |
| Home | First page |
| End | Last page |

The meaning of the left and right keys changes depending on the reading direction.

### Book navigation

| Action | Result |
| --- | --- |
| ↑ / ↓ | Book navigation |
| W / S | Book navigation |

Navigation follows the book order shown in the Library.

You can move across a mixed list of books and image folders in the same order.

### Display

| Action | Result |
| --- | --- |
| F11 | Toggle fullscreen |
| Space | Start / stop slideshow |
| ESC | Close, or exit fullscreen when fullscreen is active |

### Book actions

| Action | Result |
| --- | --- |
| Delete | Delete the current book |

Deleting an image folder removes the whole folder.

### Page range delete and archive rebuild

This operation rewrites or replaces the archive.
It is available only for archive books opened from the Library.
FolderBook, EPUB, and ImageFile are not supported.

You cannot delete all image pages.
If rebuild fails, the selected range is kept so you can retry.

Actions:

* `M`
  With no mark, mark Start using the smaller displayed page.
  With Start only, mark End using the larger displayed page.
  With Start and End, restart from the smaller displayed page.
* `Esc`
  While a range is selected, clear the current mark.
  Otherwise, keep the existing behavior.
* `Delete`
  When Start and End exist, open Delete Pages.
  Otherwise, delete the current book.
* Right-click
  Use the clicked image page as Start or End.
* Left-bottom help
  Shows `Range: S=...` / `Range: S=... E=...` while a range is selected.

### Toolbar

The toolbar provides the following actions:

* Favorites
* Display mode: AUTO / Single / Spread
* Reading direction
* Cover blank
* Quality
* Slideshow
* Fullscreen
* External Tools

Animated WebP files are played back as animations. Spread view is supported.

### Reading direction

Right-to-left and Left-to-right control the page-turn direction.

The priority is:

* Per-book setting
* Global default if the book has no override

This setting also affects the left and right keys, the progress bar, and the previous/next book cards.

### Quality

| Mode | Meaning |
| --- | --- |
| Speed | Prioritizes generation speed. |
| Balanced | Balances speed and image quality for normal use. |
| High Quality | Uses high-quality processing for the actual display size. |
| Original | Preserves the source resolution within safe limits. |

With Original, loading time, RAM usage, and GPU memory usage may increase. Fewer pages may fit in the cache.

Animation images do not use some of the quality processing.

### Language

You can switch between English and Japanese.

The default is English.

Changes take effect immediately.

---

## CUI (command line)

### Open a book

```cmd
cbz-viewer.exe "C:\path\to\sample.cbz"
```

### Open a folder of images

```cmd
cbz-viewer.exe "C:\path\to\books"
```

### Open an image file

```cmd
cbz-viewer.exe "C:\path\to\page01.jpg"
```

When you open a single supported image file, CBZ Viewer opens the parent folder as a book and starts from that image.

### Multiple instances

You can run multiple Viewer windows at the same time.

The window title shows the file name of the current book.

---

## Supported formats

### Book

* CBZ
* ZIP
* RAR
* CBR
* EPUB image books

### Image

* JPEG
* PNG
* WebP (static / animated)
* AVIF (.avif / .avifs)
* BMP
* TIFF
* GIF
