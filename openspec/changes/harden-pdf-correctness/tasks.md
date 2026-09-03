# Tasks

Ordering matters: the shared accessors in group 1 are what groups 2–4 depend on, and the fixture corpus in group 9 is the proof for the rest. Each fixture task names the finding it pins down.

## 1. Shared PDF object access

- [x] 1.1 Add `pdf::effective_page_resources(doc, page_id) -> Result<Dictionary, _>` resolving a page's `/Resources` when direct, when an indirect reference, and when inherited from any `Pages` ancestor, merging child over parent so a page's own entry wins over an inherited one of the same name
- [x] 1.2 Route all six current resource reads through it: `normalize::nest_page`, `normalize::split_spread_pages`, `cover::copy_page_as_form`, `cover::extract_panel_as_preview_page`, `ctm_walk::walk_page_images` (including the form's own `/Resources`, which currently falls back to the page's), and `preflight`'s reads
- [x] 1.3 Make `pdf::rotation_degrees` dereference and accept `Real` as well as `Integer`, round to the nearest 90, and return a distinguishable "present but unreadable" or "not a multiple of 90" outcome instead of `unwrap_or(0)`
- [x] 1.4 Make `pdf::own_box_rect` / `as_rect_points` dereference both the box entry and each element of the array
- [x] 1.5 Add a shared accessor for the catalog's `/Names` tree that resolves an indirect reference and supports mutation through the referenced object, and use it in both `preflight::check_structure` and `normalize::sanitize_structure`
- [x] 1.6 Add a visited-set (or depth cap matching lopdf's page-tree limit) to `pdf::get_inherited` so a `/Parent` cycle terminates
- [x] 1.7 Convert `pdf::deep_copy_object` from recursion to an explicit worklist, or give it a depth budget, so a long reference chain cannot overflow the stack
- [x] 1.8 Fix `pdf::deep_copy_object` to keep the original `/Filter` and `/DecodeParms` and copy the bytes verbatim when `get_plain_content` fails, dropping the filter only when decoding actually succeeded
- [x] 1.9 Make `pdf::apply_deterministic_identity` report rather than swallow the case where `/Info` resolves to a non-dictionary, since silently skipping it defeats the reproducibility guarantee the function exists for
- [x] 1.10 Report a dangling `Object::Reference` encountered during a deep copy as a finding instead of silently substituting `Object::Null`
- [x] 1.11 Guard `units::Length` against `NaN` construction, and make `Rect::inset` refuse to produce an inverted rectangle

## 2. Preflight sees through nesting

- [x] 2.1 Make `check_font_embedding` discover fonts through effective page resources and through every form XObject the page draws, reusing `ctm_walk`'s traversal rather than adding a second one
- [x] 2.2 Make `check_colour_and_ink` evaluate the content and resources of nested form XObjects as well as the page's own
- [x] 2.3 Add an operation budget to `ctm_walk` on top of `MAX_FORM_DEPTH`, and surface budget exhaustion as a blocking finding stating the checks are incomplete
- [x] 2.4 Add a blocking finding for a content stream naming a resource that cannot be resolved, keyed on the named operand rather than merely on an empty resource dictionary, so a genuinely blank page is not flagged
- [x] 2.5 Add a blocking finding for a page whose geometry cannot be resolved, and stop `continue`-skipping such pages in `check_page_size_matches_target` and `check_mixed_page_sizes`
- [x] 2.6 Add a finding for a `/Rotate` that is present but unreadable, or not a multiple of 90
- [x] 2.7 Give the low-tint finding its own code (`COLOUR_LOW_TINT`) rather than reusing `COLOUR_UNSUPPORTED_SPACE`, and move every bare string literal code into the `codes` registry
- [x] 2.8 Wire `geometry::interior_safety_margin` into an actual check of interior content against the safe area, or remove it

## 3. Normalization correctness

- [x] 3.1 Use effective page resources when building the nested form, and refuse to substitute an empty dictionary
- [x] 3.2 Preflight the input as well as the output in `normalize_interior`, carrying forward every input finding that was not fixed, deduplicated by code plus page set
- [x] 3.3 Ensure a run carrying an unfixed blocking finding is not reported as print-ready and drives exit 1, so `check` and `interior` cannot disagree about the same file
- [x] 3.4 Refuse a non-finite or non-positive dimension in `fit_placement`, and assert every `cm` operand finite immediately before writing it
- [x] 3.5 Deduplicate or split page objects aliased more than once in `/Kids`, so a shared page is not nested repeatedly with compounding transforms and conflicting parity shifts
- [x] 3.6 Report when the gutter shift moves content outside the trim or safety rectangle
- [x] 3.7 Either implement `FitMode::StretchMargins`'s documented bleed fill or reject the mode as unimplemented; it must not remain a silent alias for `Center`
- [x] 3.8 Resolve the `/Names` tree when sanitizing, and make `SanitizeSummary` reflect what was actually removed

## 4. Cover geometry

- [x] 4.1 Carry the trim rectangle explicitly on `CoverGeometry`, computed by the geometry builder that knows the binding's overhang (0.125 in bleed for perfect binding, 0.875 in board overhang for case wrap)
- [x] 4.2 Draw the template's trim guide from that rectangle, and write it as the page's `TrimBox`/`ArtBox`, replacing the hardcoded `canvas.inset(bleed())` at both sites
- [x] 4.3 Inset safety guides from the trim rectangle rather than from the panel rectangle
- [x] 4.4 Skip or report a degenerate safety inset instead of drawing an inverted rectangle
- [x] 4.5 Call `geometry::spine_too_narrow_for_text` from the cover path and emit its warning
- [x] 4.6 Measure supplied cover artwork by its effective (rotation-applied) size, and report a height mismatch as well as a width mismatch
- [x] 4.7 Return an error for a supplied cover with no pages instead of `expect`-panicking, and map it to exit 3
- [x] 4.8 In `assemble_three_panel_cover`, clip each panel's form to its destination panel rect, align outer panels to the canvas edge rather than centring, and report a panel-size mismatch

## 5. Lulu API path

- [x] 5.1 Make `hardcover_geometry_via_api` refuse `LinenWrap`, returning `HardcoverGeometryUnavailable`, so the API path cannot reintroduce the dust-jacket guess `cover.rs` refuses to make
- [x] 5.2 Route the binding-to-spine-rule decision through one shared function used by both `cover_geometry` and the API path
- [x] 5.3 Return an error rather than `Length::ZERO` for an unexpected `SpineWidth` in the API path

## 6. External tools and pipeline

- [x] 6.1 Apply a timeout to `repair_with_qpdf` and `flatten_with_ghostscript`, which currently use `Command::output()` unbounded
- [x] 6.2 Drain child stdout/stderr concurrently with execution in the shared runner, so a chatty child cannot deadlock
- [x] 6.3 Report qpdf's own diagnostics when repair was attempted and failed, instead of discarding them for the original parse error
- [x] 6.4 Record in the report that the input was repaired, replacing `let _ = was_repaired`
- [x] 6.5 Re-preflight the final bytes after the Ghostscript stage so the reported verdict describes the file actually written
- [x] 6.6 Make `assert_geometry_preserved` compare four numbers per box explicitly and fail when either side is unreadable, rather than passing vacuously on two empty vectors
- [x] 6.7 Collapse `load_with_optional_repair` and `repair_bytes_if_needed` so one delegates to the other

## 7. CLI contracts

- [x] 7.1 Emit one combined report document for `book` (`{interior, cover}`) so `--json` is parseable and `--report-out` is not truncated by the second write
- [x] 7.2 Propagate `write_output`'s computed exit code instead of flattening every failure to 2
- [x] 7.3 Fail with exit 2 on an unparseable option value at any precedence layer, naming the option, the value, and the accepted values, rather than falling through to a default
- [x] 7.4 Wire `--gutter-floor-in` into the gutter advisory threshold, or remove the flag; `--print-config` must not display a value nothing reads
- [x] 7.5 Decide `--no-color`'s fate: either apply it to real colour output or remove it, so the flag is not inert
- [x] 7.6 Use the full SKU rather than `file_stem` when deriving a filename from a product identifier, so `.MXX` and `.GXX` cannot collide
- [x] 7.7 Validate `--doc-id` bytewise (`is_ascii_hexdigit`) so a multi-byte character cannot panic the slice, and report when only one of `--doc-id`/`--creation-date` is supplied
- [x] 7.8 Return exit 2 for a non-UTF-8 input or output path instead of `expect`-panicking
- [x] 7.9 Apply the same strict argument validation to `products` that `build_selector` applies elsewhere, so `--trim 6*9` errors rather than silently listing the whole catalog
- [x] 7.10 Surface `pod_package_id`'s legacy-SKU `DeprecationNotice` when a legacy 27-character SKU is used, or remove the module; it is currently unreachable from any command

## 8. Documentation and consistency

- [x] 8.1 Fix `pipeline.rs`'s stale module doc claiming spread splitting has no implementation
- [x] 8.2 Fix `icc.rs`'s doc claiming a Flate filter is re-applied on output when the code writes raw samples; validate sample-length against width × height × channels, and state that `/Decode`, `/SMask`, and `/ImageMask` are unhandled
- [x] 8.3 Collapse the three encodings of "which bindings have a spine" (`Binding::has_spine`, `cover_geometry`'s match, `spine_width`'s match) to one
- [x] 8.4 Mask `detected_tools[].path` and `tool_version` in `normalized_for_diff`, or document why two machines are expected to differ
- [x] 8.5 Reject `multiple == 0` and use `checked_add` in `PageCountRules::next_conformant`, or make its fields private
- [x] 8.6 Update the README where behaviour changes (fit modes, the new blocking findings, `book`'s report shape)

## 9. Test corpus for the shapes that hid these bugs

- [ ] 9.1 Add fixtures for indirect `/Resources` and inherited `/Resources`, each with content that references a font, asserting the text survives normalization (finding 1)
- [ ] 9.2 Add a fixture whose page and `Pages` ancestor define the same resource name, asserting the page's own entry wins
- [ ] 9.3 Add a test asserting `check` and `interior` agree on `unembedded_font.pdf`: both blocking, both exit 1 (finding 2)
- [ ] 9.4 Add fixtures for `/Rotate` as a real number and as an indirect reference, asserting the effective size and the baked rotation (finding 3)
- [ ] 9.5 Add a `/Parent` cycle fixture with a bounded-time assertion (finding 4)
- [ ] 9.6 Add an indirect-`/MediaBox` fixture, asserting a blocking finding rather than a silently skipped page (finding 5)
- [ ] 9.7 Add a zero-dimension page fixture, asserting refusal and that no `NaN` reaches any output (finding 6)
- [ ] 9.8 Add a case-wrap template assertion pinning `TrimBox` to the 63 pt inset and the safety guides to a further 54 pt (finding 7)
- [ ] 9.9 Add a `DCTDecode` image fixture, asserting the filter and bytes survive a deep copy (finding 8)
- [ ] 9.10 Add a zero-page supplied-cover test asserting a clean error and no panic (finding 10)
- [ ] 9.11 Add an indirect-`/Names` fixture carrying JavaScript, asserting it is both reported and removed (finding 11)
- [ ] 9.12 Add a long reference-chain fixture, asserting a finding rather than a stack overflow (finding 19)
- [ ] 9.13 Add a form-XObject-with-indirect-resources fixture, asserting the correct image is found and its PPI is right (finding 20)
- [ ] 9.14 Add an aliased-page fixture (one page object in `/Kids` twice), asserting each output page is nested exactly once
- [ ] 9.15 Add CLI tests for `book --json` parseability, `--report-out` completeness, the write-failure exit code, invalid config values, and the full-SKU filename
- [ ] 9.16 Regenerate the affected snapshots and review the diff line by line, treating it as the evidence that the checks now see through nesting rather than blanket-accepting it
- [ ] 9.17 Extend `examples/generate_fixtures.rs` to build every fixture above, so the corpus stays reproducible from one command
