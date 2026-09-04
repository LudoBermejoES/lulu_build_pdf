# Uploading books to Lulu — playbook and discoveries

This documents everything learned while batch-uploading the Mage 20th
Anniversary Convention/Tradition Books to lulu.com via browser automation
(`claude-in-chrome`), plus how to diagnose and repair source PDFs that Lulu's
validator rejects. Read this before doing another Lulu upload batch.

## The SKU / spec used for these books

Product: `0850X1100.BW.STD.PB.060UW444.MXX`, which corresponds to these
human-readable choices in Lulu's wizard:

- Book Size: US Letter (8.5 x 11 in / 216 x 279 mm) — auto-derived from the
  interior PDF's page size, not chosen explicitly
- Interior Color: **Standard Black & White**
- Paper Type: **60# White — Uncoated**
- Binding Type: **Paperback Perfect Bound**
- Cover Finish: **Glossy**

Run `lulu-prep check --sku 0850X1100.BW.STD.PB.060UW444.MXX <file>` to
preflight a file against this exact spec before ever touching the browser.

## The wizard flow (per book)

URL pattern: `https://www.lulu.com/account/wizard/<project-id>/<step>` where
`<step>` is `start`, `design`, or `review`. Steps show as tabs at the top
(`Start / Design / Review`) — a green checkmark means that step is complete.

1. **Start**: `https://www.lulu.com/account/wizard/draft/start` creates a new
   draft project the instant you type a title (the URL redirects to a real
   project ID). Fill in:
   - Project Title (free text — see naming convention below)
   - **Book category** — REQUIRED even though it has no asterisk. Typing
     "Games" and selecting the one dropdown match is enough. Skipping this
     silently leaves the Start step incomplete and blocks Review later with
     no obvious error until you dig for it.
   - "Print Your Book" goal is selected by default — don't need to touch it.
   - Click "Design Your Project" to continue.
2. **Design**: upload the interior PDF, pick the four spec radios (Standard
   B&W / 60# White Uncoated / Paperback Perfect Bound / Glossy), upload the
   cover PDF.
3. **Review**: shows the cover thumbnail + specs. Click "Confirm and
   Publish". This finalizes the project to `COMPLETE` / `PRIVATE ACCESS` —
   **no payment or public listing happens** at this step; it's equivalent to
   saving a private draft you can order later. Sometimes the project reaches
   `COMPLETE` just by navigating to Review with everything already valid,
   without ever needing the Confirm click — don't be surprised either way.

**Replacing the interior file resets the Design step's specs AND the
uploaded cover.** After swapping in a fixed/revised interior PDF, you must
reselect all four spec radios and re-upload the cover from scratch — nothing
carries over.

## Naming convention used

`"Convention Book: <Name>"` / `"Tradition Book: <Name>"` — colon-separated,
title-cased, matching the pre-existing "Convention Book: Iteration X
Revised" project's style (dropping "Revised"/"-interior" suffixes from
filenames).

## The 10MB upload wall — and how to get past it for large interiors

The claude-in-chrome `file_upload` tool caps combined upload size at **10MB
per call** — this is a limit of the browser-automation message channel
itself, not of Lulu (Lulu accepts files well over 200MB through its own
upload widget). Covers here were all under 3MB and uploaded fine directly
via `file_upload`. Interior PDFs ranged 19–203MB and always exceeded the cap.

**Fix: have the browser fetch the file itself, bypassing the extension's
message channel entirely.**

1. Start a tiny local CORS-enabled HTTP server serving the directory with
   the PDFs (see `cors_server.py` pattern below — plain `http.server`
   doesn't send `Access-Control-Allow-Origin`, so a custom handler is
   needed):

   ```python
   import http.server, socketserver, sys
   PORT = int(sys.argv[1]); DIRECTORY = sys.argv[2]
   class Handler(http.server.SimpleHTTPRequestHandler):
       def __init__(self, *a, **kw): super().__init__(*a, directory=DIRECTORY, **kw)
       def end_headers(self):
           self.send_header("Access-Control-Allow-Origin", "*")
           self.send_header("Cache-Control", "no-store")
           super().end_headers()
   class ThreadingHTTPServer(socketserver.ThreadingMixIn, http.server.HTTPServer):
       daemon_threads = True
   ThreadingHTTPServer(("127.0.0.1", PORT), Handler).serve_forever()
   ```

   Run it bound to `127.0.0.1` only, for the duration of the session, then
   kill it (`pkill -f cors_server.py`) when done.

2. In the Lulu tab, run this via `javascript_tool` (NOT the `file_upload`
   tool, and NOT a real click on the upload button — clicking opens a native
   OS file picker that browser automation cannot see or interact with at
   all, on any client):

   ```js
   const resp = await fetch("http://127.0.0.1:8765/My%20File.pdf");
   const blob = await resp.blob();
   const file = new File([blob], "My File.pdf", { type: "application/pdf" });
   const dt = new DataTransfer();
   dt.items.add(file);
   const input = document.querySelectorAll('input[type=file]')[0];
   input.files = dt.files;
   input.dispatchEvent(new Event('change', { bubbles: true }));
   input.dispatchEvent(new Event('input', { bubbles: true }));
   ```

   This is the same technique Playwright/Cypress use internally for
   `setInputFiles`. The bytes move over normal browser networking, never
   through the automation channel, so there's no size limit.

   Small files (covers) can still go through the `file_upload` tool
   normally — it's simpler when it fits.

3. After upload, Lulu shows a "Your file is validating" / "normalizing"
   progress bar (0–100%). For large files this can take 30–60+ seconds
   across two phases (validating, then normalizing). **It sometimes visibly
   sticks at 0% or 100%** — reload the page
   (`navigate` to the same `.../design` URL) before assuming it's broken;
   about half the time this reveals real ongoing progress the client-side
   UI just failed to reflect. If a genuine stall persists after reload +
   wait, re-run the same JS injection to re-upload — this reliably unsticks
   it.

## Diagnosing a file Lulu's validator rejects outright

Lulu's own error is a single opaque line: *"We've found an error in your
PDF and can't automatically repair it... Error Message: Error in PDF
syntax."* It gives no location. `lulu-prep check` did not catch this either
in one real case — the two tools check different things. Reach for `qpdf
--check` first:

```
qpdf --check "file.pdf"
```

- `invalid jpeg data reading from buffer` warnings, one per object, mean
  specific image streams are malformed. **This is what actually blocks
  Lulu** — even a single such warning anywhere in the file causes the hard
  rejection, regardless of whether that image is visually load-bearing.
- Extract one and inspect it directly to characterize the damage:
  ```
  qpdf --show-object=<N> "file.pdf"                       # dictionary: /Length, /Width, /Height...
  qpdf --show-object=<N> --raw-stream-data "file.pdf" > /tmp/obj.jpg
  identify /tmp/obj.jpg                                     # ImageMagick — reports exactly what's wrong
  ```
  In the case investigated here, every corrupted stream's actual byte count
  matched its declared `/Length` exactly (e.g. 162 bytes) — i.e. the file
  faithfully records a *truncated* JPEG (headers + partial quantization/
  Huffman tables, zero scan data). This is unrecoverable from the bytes
  present; it is not bit-rot or a transfer glitch, it's baked into the
  source file at its origin.
- Not every corrupted image blanks its page — some are unused SMasks or
  secondary layers under otherwise-intact content. To find which pages are
  **actually** visually blank (the real damage), render everything at low
  res and histogram it:
  ```python
  from PIL import Image
  # pdftoppm -r 40 -gray -png file.pdf /tmp/out/page   (run first)
  for f in sorted(glob.glob("/tmp/out/page-*.png")):
      img = Image.open(f).convert("L")
      white_frac = sum(img.histogram()[240:256]) / sum(img.histogram())
      if white_frac > 0.995: print(f, "BLANK")
  ```
  Cross-reference blank page numbers against the book's own table of
  contents — in the real case here, all 4 genuinely blank pages turned out
  to be exactly the chapter-opener splash-art pages.
- Check whether other copies of the same source exist (Downloads, Trash,
  differently-named re-uploads) with `md5`/`ls -la` before concluding a
  repair is necessary — a byte-identical MD5 across every copy on disk means
  the defect is in the one-and-only source, not introduced by anything
  local. Conversely, a **new, differently-sized download can turn out to be
  a clean copy** — always check `Downloads` for a fresher file before doing
  repair work; that's exactly what happened here (`ilide.info-...pr_13d7...`,
  9.7MB, replaced a 14.8MB corrupt copy and had zero `qpdf --check` errors).

## Fixing unembedded-font blocking findings (OCR text layers)

A second, unrelated failure mode: `lulu-prep check` blocking on
`fonts.not-embedded` for names like `ArialMT`, `Courier`, `CourierNewPSMT`,
`Impact`. Check with `pdffonts file.pdf` — if the flagged fonts show
`emb=no sub=no uni=yes` and are exactly these classic standard-font names,
it's almost certainly an **invisible OCR searchable-text layer** (added by
whatever scanned/OCR'd the source), not visible body text — cosmetically
harmless, but Lulu's parser blocks on it regardless of visibility.

Fix by running the file through Ghostscript's `pdfwrite` device, which
re-embeds every font it uses:

```
lulu-prep interior --sku <SKU> --flatten --force <input.pdf>
```

`--flatten` requires `gs` on PATH (`brew install ghostscript` if missing —
it is NOT installed by default on this machine). This does **not**
rasterize the page to an image; it redistills the PDF (vector content and
images both preserved, `-dDownsample*=false`), while embedding/subsetting
fonts it touches.

**Caveat**: Ghostscript's `-dEmbedAllFonts=true` still refuses to embed the
14 PDF standard fonts (`Courier`, `Helvetica`, `Times-*`, `Symbol`,
`ZapfDingbats`) by design — it assumes any conformant reader has them. If
`Courier` specifically survives as still-unembedded after a `--flatten`
pass, force it with a second manual Ghostscript pass adding a distiller
param override (the plain `-dNeverEmbed=[]` CLI flag form errors out; it
must go through `-c`/`-f`):

```
gs -dNOPAUSE -dBATCH -dSAFER -sDEVICE=pdfwrite \
   -dEmbedAllFonts=true -dSubsetFonts=true \
   -dDownsampleColorImages=false -dDownsampleGrayImages=false -dDownsampleMonoImages=false \
   -dAutoRotatePages=/None \
   -sOutputFile=out.pdf \
   -c "<< /NeverEmbed [ ] >> setdistillerparams" \
   -f in.pdf
```

Re-run `lulu-prep check` after any Ghostscript pass — confirm `0 blocking
findings` before touching Lulu again.

## Operational notes / gotchas

- The wizard's top-of-page nav has two rows that look similar at a glance:
  the account nav (`My Projects / My Stores / My Account`, ~y=27) and the
  wizard step nav (`Start / Design / Review`, ~y=81 when both are visible,
  but the account nav collapses away — leaving `Start/Design/Review` at
  y=23 — once you've scrolled). Double-check which one you're clicking;
  misclicking lands on `My Projects` and looks like nothing happened.
- The `claude-in-chrome` extension can silently disconnect mid-session
  (transient — Chrome service worker restart) and reconnect on its own
  within a few seconds; if a tool call fails with "extension disconnected",
  just retry rather than assuming something broke.
- `lulu-prep check`'s SKU flag and the browser wizard's spec radios must
  agree, or you're preflighting against the wrong target. Keep the SKU
  string in one place (see top of this doc) and reuse it verbatim in both.
- Source PDFs named `ilide.info-*` are from a scanned-document sharing
  site, not an official/paid source — worth treating any defect found in
  such a file as unsurprising, and worth checking for a legitimate copy if
  one is available.
