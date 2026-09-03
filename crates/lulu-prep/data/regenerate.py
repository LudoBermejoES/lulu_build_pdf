#!/usr/bin/env python3
"""Regenerate pod-packages.csv from Lulu's published product spec sheet.

Usage: python3 regenerate.py [path-to-cached-xlsx]

Downloads (or reads a cached copy of) Lulu's spec sheet and writes
`pod-packages.csv` next to this script, with a header comment recording
the source URL and the fetch date.
"""
import csv
import datetime
import re
import sys
import urllib.request
import zipfile
from pathlib import Path

SOURCE_URL = "https://assets.lulu.com/media/specs/lulu-print-api-spec-sheet.xlsx"
HERE = Path(__file__).parent
OUT_PATH = HERE / "pod-packages.csv"

COLUMNS = [
    "legacy_sku", "sku", "book_type", "min_page", "max_page",
    "trim_width_in", "trim_height_in", "trim_width_mm", "trim_height_mm",
    "bleed_width_in", "bleed_height_in", "bleed_width_mm", "bleed_height_mm",
    "interior_color", "print_quality", "bind", "interior_number",
    "paper_type", "interior_ppi", "lamination", "linen_color", "foil_color",
]


def _col_letters_to_index(ref: str) -> int:
    """'AC2' -> the zero-based column index of 'AC'. XLSX omits empty cells from
    the XML entirely (sparse rows), so columns must be located by this reference,
    not by position — a naive positional read silently shifts every later column
    left by one whenever an earlier cell in the row is blank."""
    letters = re.match(r"[A-Z]+", ref).group()
    index = 0
    for ch in letters:
        index = index * 26 + (ord(ch) - ord("A") + 1)
    return index - 1


def load_xlsx_rows(xlsx_path: Path):
    z = zipfile.ZipFile(xlsx_path)
    sst = []
    try:
        sst_xml = z.read("xl/sharedStrings.xml").decode()
        sst = [re.sub(r"<[^>]+>", "", m) for m in re.findall(r"<si>(.*?)</si>", sst_xml, re.S)]
    except KeyError:
        pass

    sheet_xml = z.read("xl/worksheets/sheet2.xml").decode()  # "Full Spec Sheet"
    rows = []
    for r in re.findall(r"<row[^>]*>(.*?)</row>", sheet_xml, re.S):
        sparse = {}
        max_index = -1
        for cm in re.finditer(r'<c\b([^>]*?)(?:/>|>(.*?)</c>)', r, re.S):
            attrs = cm.group(1) or ""
            body = cm.group(2) or ""
            ref = re.search(r'r="([A-Z]+\d+)"', attrs)
            if not ref:
                continue
            index = _col_letters_to_index(ref.group(1))
            t = re.search(r't="(\w+)"', attrs)
            v = re.search(r"<v>(.*?)</v>", body, re.S)
            val = v.group(1) if v else ""
            if t and t.group(1) == "s" and val:
                val = sst[int(val)]
            sparse[index] = val
            max_index = max(max_index, index)
        cells = [sparse.get(i, "") for i in range(max_index + 1)]
        rows.append(cells)
    return rows


def main():
    if len(sys.argv) > 1:
        xlsx_path = Path(sys.argv[1])
    else:
        xlsx_path = HERE / "_source.xlsx"
        print(f"Downloading {SOURCE_URL} ...", file=sys.stderr)
        urllib.request.urlretrieve(SOURCE_URL, xlsx_path)

    rows = load_xlsx_rows(xlsx_path)
    header = rows[0]

    def col(name):
        return header.index(name)

    idx = {
        "legacy_sku": col("Legacy SKU - Deprecated February 1, 2027"),
        "sku": col("New SKU - March 31 2026"),
        "book_type": col("Book Type"),
        "min_page": col("Min Page"),
        "max_page": col("Max Page"),
        "trim_width_in": col("Trim Width (in)"),
        "trim_height_in": col("Trim Height (in)"),
        "trim_width_mm": col("Trim Width (mm)2"),
        "trim_height_mm": col("Trim Height (mm)"),
        "bleed_width_in": col("Width w/ Bleed (in)"),
        "bleed_height_in": col("Height w/ Bleed (in)"),
        "bleed_width_mm": col("Width w/ Bleed (mm)"),
        "bleed_height_mm": col("Height w/ Bleed (mm)"),
        "interior_color": col("Interior Color"),
        "print_quality": col("Print Quality"),
        "bind": col("Bind"),
        "interior_number": col("Interior (#)"),
        "paper_type": col("Paper Type"),
        "interior_ppi": col("Interior PPI"),
        "lamination": col("Lamination"),
        "linen_color": col("Linen Color"),
        "foil_color": col("Foil Color"),
    }

    out_rows = []
    for r in rows[1:]:
        if not r or not r[idx["sku"]]:
            continue
        out_rows.append([html_unescape(r[idx[c]]) if idx[c] < len(r) else "" for c in COLUMNS])

    fetch_date = datetime.date.today().isoformat()
    with OUT_PATH.open("w", newline="") as f:
        f.write(f"# source: {SOURCE_URL}\n")
        f.write(f"# fetched: {fetch_date}\n")
        f.write(f"# products: {len(out_rows)}\n")
        w = csv.writer(f)
        w.writerow(COLUMNS)
        w.writerows(out_rows)

    print(f"Wrote {len(out_rows)} products to {OUT_PATH}", file=sys.stderr)


def html_unescape(s: str) -> str:
    return (
        s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", '"')
        .replace("&apos;", "'")
    )


if __name__ == "__main__":
    main()
