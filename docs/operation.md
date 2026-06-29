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

The Settings window has five tabs: General, Library, Viewer, Performance, and External Tools.

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

### Performance

* L1 VRAM Cache: Stores GPU textures for upcoming and recently viewed pages.
* L2 RAM Cache: Stores decoded RGBA images in system memory.
* Background Workers: Controls background decoding parallelism.
* Danger Zone: Allows manual values beyond the normal hardware-based limits.

---

## Library screen

### Basic actions

| Action | Result |
| --- | --- |
| Double-click | Open book |
| Enter | Open book |
| Delete | Delete selected book |
| F2 | Rename |
| Ctrl+A | Select all |
| Ctrl+C | Copy path |

### Search

Use the search box to filter by book title.

### Favorites

You can show only books that are marked as favorites.

### Reading status

You can filter books by Unread / Reading / Read. Counts are shown.

Reading status updates automatically from Viewer progress. Closing a book before the end marks it as Reading, and closing it after showing the last page marks it as Read.

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
| External Tools | Run external tools |

When a FolderBook is selected, `Move to folder` is also shown.

The selected book file or folder is deleted, and related thumbnails, Page Map, favorites, and groups are removed.

Some items are hidden or disabled when multiple items are selected.

---

## Viewer screen

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
  Shows `Delete Range: S=... E=...` while a range is selected.

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
