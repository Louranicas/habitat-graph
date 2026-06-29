//! `PDF` text ingestion (`PE`, feature `pdf`) — local-first, `DoS`-capped.
//!
//! [`extract_text`] turns `PDF` bytes into plain text via the `pdf-extract` crate,
//! behind a hard size cap so an oversized input cannot exhaust memory.  The call is
//! wrapped in [`std::panic::catch_unwind`] because `pdf-extract` contains many raw
//! `unwrap`/`expect`/`panic!` call sites that fire on malformed or encrypted inputs;
//! we convert any such panic into a [`habitat_graph_core::GraphError::Parse`] so the
//! process never crashes on hostile data.
//!
//! **No network I/O** is performed (local-first, R3).  The entire module is
//! feature-gated behind `pdf` so the default build pulls no `PDF` dependency.

use habitat_graph_core::{GraphError, Result};

/// Maximum accepted `PDF` **input** size in bytes — a first-line `DoS` guard.
///
/// Inputs larger than this constant are rejected *before* the parser is invoked. NOTE: this bounds
/// the *raw input*, not the *decompressed output* — a within-cap `PDF` with high-ratio
/// `FlateDecode`/`LZW` streams can still decompress to far more than `MAX_PDF_BYTES` (a
/// decompression bomb). For untrusted bulk input, run extraction under an OS memory/time budget
/// (`RLIMIT_AS` / cgroup) in addition to this cap. The feature is off by default and intended for a
/// local-first corpus.
pub const MAX_PDF_BYTES: usize = 64 * 1024 * 1024; // 64 MiB

/// Extracts plain text from the `PDF` bytes in `bytes`.
///
/// Extraction is performed by `pdf-extract` and is wrapped in
/// [`std::panic::catch_unwind`] so that any panic inside the upstream crate
/// (which contains many internal `unwrap`/`panic!` call sites) is caught and
/// converted to an error rather than crashing the process.
///
/// > **Note:** `catch_unwind` cannot intercept aborts compiled under
/// > `panic = "abort"` profiles.
///
/// # Errors
///
/// * [`GraphError::Io`] — `bytes.len()` exceeds [`MAX_PDF_BYTES`].
/// * [`GraphError::Parse`] — the bytes are not a parseable `PDF`, or
///   `pdf-extract` panics while processing them.
pub fn extract_text(bytes: &[u8]) -> Result<String> {
    if bytes.len() > MAX_PDF_BYTES {
        return Err(GraphError::Io(format!(
            "pdf exceeds {MAX_PDF_BYTES}-byte cap ({} bytes)",
            bytes.len()
        )));
    }

    // Wrap extraction in `catch_unwind`: `pdf-extract` contains many raw
    // `unwrap`/`expect`/`panic!` calls that fire on malformed, encrypted, or
    // otherwise pathological PDFs (e.g., `get_catalog` panics when the trailer
    // `Root` entry does not resolve to a `Dictionary`).  We convert any panic
    // payload into a `GraphError::Parse` so a hostile PDF cannot crash the
    // process.
    //
    // `bytes` is `&[u8]`, which is `RefUnwindSafe` (blanket impl for shared
    // references to `RefUnwindSafe` types), so the closure is `UnwindSafe`
    // without an explicit `AssertUnwindSafe` wrapper.
    match std::panic::catch_unwind(|| pdf_extract::extract_text_from_mem(bytes)) {
        Err(_panic_payload) => Err(GraphError::Parse {
            file: "<pdf>".to_owned(),
            message: "pdf-extract panicked on malformed input".to_owned(),
        }),
        Ok(pdf_result) => pdf_result.map_err(|e| GraphError::Parse {
            file: "<pdf>".to_owned(),
            message: e.to_string(),
        }),
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use habitat_graph_core::GraphError;

    use super::{extract_text, MAX_PDF_BYTES};

    // ── Helpers ──────────────────────────────────────────────────────────────

    /// Build a minimal but valid `PDF` that contains `text` as a Type1/Helvetica
    /// text run.  Uses the lopdf `API` (re-exported by `pdf_extract`) so we
    /// don't need to hard-code byte offsets.
    fn build_pdf_with_text(text: &str) -> Vec<u8> {
        use pdf_extract::{Dictionary, Document, Object, Stream};

        let mut doc = Document::with_version("1.4");

        // Type1 Helvetica font with `WinAnsiEncoding` so `ASCII` text is
        // extractable via the standard glyph-name table.
        let mut font = Dictionary::new();
        font.set("Type", Object::Name(b"Font".to_vec()));
        font.set("Subtype", Object::Name(b"Type1".to_vec()));
        font.set("BaseFont", Object::Name(b"Helvetica".to_vec()));
        font.set("Encoding", Object::Name(b"WinAnsiEncoding".to_vec()));
        let font_id = doc.add_object(Object::Dictionary(font));

        // Content stream.
        let content = format!("BT /F1 12 Tf 72 720 Td ({text}) Tj ET");
        let content_bytes = content.into_bytes();
        let stream_len =
            i64::try_from(content_bytes.len()).expect("content length fits i64");
        let mut sd = Dictionary::new();
        sd.set("Length", Object::Integer(stream_len));
        let stream_id =
            doc.add_object(Object::Stream(Stream::new(sd, content_bytes)));

        // Resources dictionary.
        let mut font_res = Dictionary::new();
        font_res.set("F1", Object::Reference(font_id));
        let mut resources = Dictionary::new();
        resources.set("Font", Object::Dictionary(font_res));

        // Single-page object.  Rename variables to avoid `similar_names` lint.
        let mut pg_obj = Dictionary::new();
        pg_obj.set("Type", Object::Name(b"Page".to_vec()));
        pg_obj.set(
            "MediaBox",
            Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(612),
                Object::Integer(792),
            ]),
        );
        pg_obj.set("Resources", Object::Dictionary(resources));
        pg_obj.set("Contents", Object::Reference(stream_id));
        let leaf_id = doc.add_object(Object::Dictionary(pg_obj));

        // Page tree (Pages) object.
        let mut tree_obj = Dictionary::new();
        tree_obj.set("Type", Object::Name(b"Pages".to_vec()));
        tree_obj.set(
            "Kids",
            Object::Array(vec![Object::Reference(leaf_id)]),
        );
        tree_obj.set("Count", Object::Integer(1));
        let tree_id = doc.add_object(Object::Dictionary(tree_obj));

        // Back-patch `Parent` into the page.
        if let Ok(obj) = doc.get_object_mut(leaf_id) {
            if let Ok(d) = obj.as_dict_mut() {
                d.set("Parent", Object::Reference(tree_id));
            }
        }

        // Catalog.
        let mut catalog = Dictionary::new();
        catalog.set("Type", Object::Name(b"Catalog".to_vec()));
        catalog.set("Pages", Object::Reference(tree_id));
        let cat_id = doc.add_object(Object::Dictionary(catalog));

        doc.trailer.set("Root", Object::Reference(cat_id));

        let mut buf = Vec::new();
        doc.save_to(&mut buf).expect("test PDF serialisation failed");
        buf
    }

    /// Build a two-page `PDF` with one text run per page.
    fn build_two_page_pdf(pg1_text: &str, pg2_text: &str) -> Vec<u8> {
        use pdf_extract::{Dictionary, Document, Object, Stream};

        let mut doc = Document::with_version("1.4");

        let mut font = Dictionary::new();
        font.set("Type", Object::Name(b"Font".to_vec()));
        font.set("Subtype", Object::Name(b"Type1".to_vec()));
        font.set("BaseFont", Object::Name(b"Helvetica".to_vec()));
        font.set("Encoding", Object::Name(b"WinAnsiEncoding".to_vec()));
        let fid = doc.add_object(Object::Dictionary(font));

        // Helper closure — not mutable.
        let make_page = |doc: &mut Document, txt: &str| {
            let content = format!("BT /F1 12 Tf 72 720 Td ({txt}) Tj ET");
            let content_bytes = content.into_bytes();
            let slen = i64::try_from(content_bytes.len()).expect("fits i64");
            let mut sd = Dictionary::new();
            sd.set("Length", Object::Integer(slen));
            let sid = doc.add_object(Object::Stream(Stream::new(sd, content_bytes)));

            let mut fr = Dictionary::new();
            fr.set("F1", Object::Reference(fid));
            let mut res = Dictionary::new();
            res.set("Font", Object::Dictionary(fr));

            let mut pg = Dictionary::new();
            pg.set("Type", Object::Name(b"Page".to_vec()));
            pg.set(
                "MediaBox",
                Object::Array(vec![
                    Object::Integer(0),
                    Object::Integer(0),
                    Object::Integer(612),
                    Object::Integer(792),
                ]),
            );
            pg.set("Resources", Object::Dictionary(res));
            pg.set("Contents", Object::Reference(sid));
            doc.add_object(Object::Dictionary(pg))
        };

        let first_id = make_page(&mut doc, pg1_text);
        let second_id = make_page(&mut doc, pg2_text);

        let mut tree = Dictionary::new();
        tree.set("Type", Object::Name(b"Pages".to_vec()));
        tree.set(
            "Kids",
            Object::Array(vec![
                Object::Reference(first_id),
                Object::Reference(second_id),
            ]),
        );
        tree.set("Count", Object::Integer(2));
        let tree_id = doc.add_object(Object::Dictionary(tree));

        for pid in [first_id, second_id] {
            if let Ok(obj) = doc.get_object_mut(pid) {
                if let Ok(d) = obj.as_dict_mut() {
                    d.set("Parent", Object::Reference(tree_id));
                }
            }
        }

        let mut cat = Dictionary::new();
        cat.set("Type", Object::Name(b"Catalog".to_vec()));
        cat.set("Pages", Object::Reference(tree_id));
        let cat_id = doc.add_object(Object::Dictionary(cat));

        doc.trailer.set("Root", Object::Reference(cat_id));

        let mut buf = Vec::new();
        doc.save_to(&mut buf)
            .expect("two-page PDF serialisation failed");
        buf
    }

    /// Build a `PDF` that contains a structurally valid page (`Type = Page`,
    /// traversable by lopdf's page iterator) but deliberately omits the
    /// `MediaBox` from both the page and its parent `Pages` node.
    ///
    /// This triggers `output_doc_inner` in `pdf-extract` to call
    /// `get_inherited(…, b"MediaBox").expect("MediaBox")` on `None`, producing
    /// a `panic!`.  `catch_unwind` in [`extract_text`] must capture it.
    fn build_panic_inducing_pdf() -> Vec<u8> {
        use pdf_extract::{Dictionary, Document, Object};

        let mut doc = Document::with_version("1.4");

        // A page dict with `Type = Page` but intentionally no `MediaBox`.
        let mut pg = Dictionary::new();
        pg.set("Type", Object::Name(b"Page".to_vec()));
        // Omit MediaBox on purpose — this is the panic trigger.
        let pg_id = doc.add_object(Object::Dictionary(pg));

        // Pages tree also has no `MediaBox` (so inherited lookup also fails).
        let mut tree = Dictionary::new();
        tree.set("Type", Object::Name(b"Pages".to_vec()));
        tree.set(
            "Kids",
            Object::Array(vec![Object::Reference(pg_id)]),
        );
        tree.set("Count", Object::Integer(1));
        let tree_id = doc.add_object(Object::Dictionary(tree));

        // Back-patch `Parent` so `get_inherited` can walk up the chain.
        if let Ok(obj) = doc.get_object_mut(pg_id) {
            if let Ok(d) = obj.as_dict_mut() {
                d.set("Parent", Object::Reference(tree_id));
            }
        }

        // Catalog.
        let mut cat = Dictionary::new();
        cat.set("Type", Object::Name(b"Catalog".to_vec()));
        cat.set("Pages", Object::Reference(tree_id));
        let cat_id = doc.add_object(Object::Dictionary(cat));

        doc.trailer.set("Root", Object::Reference(cat_id));

        let mut buf = Vec::new();
        doc.save_to(&mut buf)
            .expect("panic-inducing PDF serialisation failed");
        buf
    }

    // ── Group 1: DoS size cap ─────────────────────────────────────────────────

    /// One byte beyond the cap is rejected immediately with `GraphError::Io`.
    #[test]
    fn oversized_input_is_rejected_by_cap() {
        let big = vec![0_u8; MAX_PDF_BYTES + 1];
        let err = extract_text(&big).expect_err("oversized pdf must error");
        assert!(
            matches!(err, GraphError::Io(_)),
            "expected Io cap error, got: {err:?}"
        );
    }

    /// A buffer that is exactly one byte over the cap must fail.
    #[test]
    fn one_byte_over_cap_is_rejected() {
        let buf = vec![0_u8; MAX_PDF_BYTES + 1];
        assert!(extract_text(&buf).is_err());
    }

    /// A buffer at exactly the cap must not be rejected by the *size* guard.
    /// It may still fail (bad bytes), but the error must not be the cap guard.
    #[test]
    fn exactly_at_cap_does_not_trigger_io_size_error() {
        let buf = vec![0_u8; MAX_PDF_BYTES];
        if let Err(GraphError::Io(msg)) = extract_text(&buf) {
            assert!(
                !msg.contains("cap"),
                "zero-filled at-cap buffer should not trigger the cap guard; got: {msg}"
            );
        }
    }

    /// One byte below the cap must not trigger the cap guard.
    #[test]
    fn one_byte_under_cap_is_not_rejected_by_cap_guard() {
        let buf = vec![0_u8; MAX_PDF_BYTES - 1];
        if let Err(GraphError::Io(msg)) = extract_text(&buf) {
            assert!(
                !msg.contains("cap"),
                "below-cap buffer triggered the cap guard: {msg}"
            );
        }
    }

    /// The error for an oversized input must be `GraphError::Io`, not `Parse`.
    #[test]
    fn cap_error_variant_is_io_not_parse() {
        let big = vec![0_u8; MAX_PDF_BYTES + 1];
        let err = extract_text(&big).unwrap_err();
        assert!(
            matches!(err, GraphError::Io(_)),
            "cap error must be Io variant, got {err:?}"
        );
    }

    /// The cap error message must include the actual byte count.
    #[test]
    fn cap_error_message_contains_actual_byte_count() {
        let size = MAX_PDF_BYTES + 42;
        let big = vec![0_u8; size];
        let err = extract_text(&big).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains(&size.to_string()),
            "cap error message should contain actual size {size}: {msg}"
        );
    }

    /// The cap error message must mention the cap constant value.
    #[test]
    fn cap_error_message_mentions_cap_value() {
        let big = vec![0_u8; MAX_PDF_BYTES + 1];
        let err = extract_text(&big).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains(&MAX_PDF_BYTES.to_string()),
            "cap error should mention MAX_PDF_BYTES={MAX_PDF_BYTES}: {msg}"
        );
    }

    // ── Group 2: Empty / trivially invalid inputs ─────────────────────────────

    /// Empty slice must error, not succeed or panic.
    #[test]
    fn empty_input_errors() {
        assert!(extract_text(b"").is_err());
    }

    /// A single null byte is not a valid `PDF`.
    #[test]
    fn single_null_byte_errors() {
        assert!(extract_text(b"\x00").is_err());
    }

    /// Multiple null bytes must also error.
    #[test]
    fn multiple_null_bytes_error() {
        assert!(extract_text(&[0u8; 256]).is_err());
    }

    /// All-`0xFF` bytes are not a `PDF`.
    #[test]
    fn all_ff_bytes_error() {
        assert!(extract_text(&[0xFF_u8; 256]).is_err());
    }

    /// A lone `%` sign is not a `PDF`.
    #[test]
    fn lone_percent_sign_errors() {
        assert!(extract_text(b"%").is_err());
    }

    /// A newline-only buffer is not a `PDF`.
    #[test]
    fn newlines_only_error() {
        assert!(extract_text(b"\n\n\n\n\n").is_err());
    }

    // ── Group 3: Non-PDF formats (no panic allowed) ──────────────────────────

    /// Plain `ASCII` text is not a `PDF`.
    #[test]
    fn plain_ascii_text_errors_not_panics() {
        let result = extract_text(b"this is not a pdf at all");
        assert!(result.is_err(), "plain text must error");
    }

    /// `HTML` is not a `PDF`.
    #[test]
    fn html_document_errors_not_panics() {
        let html = b"<!DOCTYPE html><html><body>Hello</body></html>";
        assert!(extract_text(html).is_err());
    }

    /// `JSON` bytes are not a `PDF`.
    #[test]
    fn json_document_errors_not_panics() {
        let json = b"{\"key\": \"value\", \"num\": 42}";
        assert!(extract_text(json).is_err());
    }

    /// `XML` bytes are not a `PDF`.
    #[test]
    fn xml_document_errors_not_panics() {
        let xml = b"<?xml version=\"1.0\"?><root><child/></root>";
        assert!(extract_text(xml).is_err());
    }

    /// `ZIP` magic bytes (`PK\x03\x04`) are not a `PDF`.
    #[test]
    fn zip_magic_bytes_error_not_panics() {
        let zip_magic = b"PK\x03\x04\x14\x00\x00\x00\x00\x00";
        assert!(extract_text(zip_magic).is_err());
    }

    /// `JPEG`/`JFIF` magic bytes are not a `PDF`.
    #[test]
    fn jpeg_magic_bytes_error_not_panics() {
        let jpeg_magic = b"\xFF\xD8\xFF\xE0\x00\x10JFIF\x00";
        assert!(extract_text(jpeg_magic).is_err());
    }

    /// `PNG` magic bytes are not a `PDF`.
    #[test]
    fn png_magic_bytes_error_not_panics() {
        let png_magic = b"\x89PNG\r\n\x1A\n";
        assert!(extract_text(png_magic).is_err());
    }

    /// `ELF` magic bytes are not a `PDF`.
    #[test]
    fn elf_magic_bytes_error_not_panics() {
        let elf_magic = b"\x7FELF\x02\x01\x01\x00";
        assert!(extract_text(elf_magic).is_err());
    }

    /// Random binary data must error, not panic.
    #[test]
    fn random_binary_data_errors_not_panics() {
        let binary: Vec<u8> = (0u8..=255).collect();
        assert!(extract_text(&binary).is_err());
    }

    /// A Rust source file fragment is not a `PDF`.
    #[test]
    fn rust_source_code_errors_not_panics() {
        let src = b"fn main() { println!(\"Hello, world!\"); }";
        assert!(extract_text(src).is_err());
    }

    // ── Group 4: PDF header present but body invalid ─────────────────────────

    /// Only the `PDF` header marker with no further structure must error.
    #[test]
    fn pdf_header_only_errors() {
        assert!(extract_text(b"%PDF-1.4").is_err());
    }

    /// Header + `%%EOF` with no objects or xref must error.
    #[test]
    fn pdf_header_and_eof_only_errors() {
        assert!(extract_text(b"%PDF-1.4\n%%EOF\n").is_err());
    }

    /// `PDF` header followed by random garbage must error.
    #[test]
    fn pdf_header_then_random_garbage_errors() {
        let mut bytes = b"%PDF-1.4\n".to_vec();
        bytes.extend_from_slice(&[0xCA, 0xFE, 0xBA, 0xBE, 0xDE, 0xAD, 0xBE, 0xEF]);
        assert!(extract_text(&bytes).is_err());
    }

    /// A `PDF` header followed by `1 0 obj` with no closing must error.
    #[test]
    fn pdf_with_truncated_object_errors() {
        let truncated = b"%PDF-1.4\n1 0 obj\n<< /Type /Catalog";
        assert!(extract_text(truncated).is_err());
    }

    /// A `PDF` with `startxref` pointing past the end of the buffer must error.
    #[test]
    fn pdf_with_bad_xref_offset_errors() {
        let bad =
            b"%PDF-1.4\ntrailer\n<< /Size 1 /Root 1 0 R >>\nstartxref\n999999\n%%EOF\n";
        assert!(extract_text(bad).is_err());
    }

    /// `PDF` version 1.0 header with garbage body errors cleanly.
    #[test]
    fn pdf_version_10_with_garbage_errors() {
        let mut bytes = b"%PDF-1.0\n".to_vec();
        bytes.extend_from_slice(&[0u8; 64]);
        assert!(extract_text(&bytes).is_err());
    }

    /// `PDF` 2.0 header with garbage body errors cleanly.
    #[test]
    fn pdf_version_20_with_garbage_errors() {
        let mut bytes = b"%PDF-2.0\n".to_vec();
        bytes.extend_from_slice(&[0u8; 64]);
        assert!(extract_text(&bytes).is_err());
    }

    /// A truncated stream declaration must error.
    #[test]
    fn pdf_with_truncated_stream_errors() {
        let truncated =
            b"%PDF-1.4\n5 0 obj\n<< /Length 100 >>\nstream\nhello\nendstream\n%%EOF\n";
        assert!(extract_text(truncated).is_err());
    }

    /// An empty trailer dictionary must produce an error, not a panic.
    #[test]
    fn pdf_with_empty_trailer_errors_not_panics() {
        let bad =
            b"%PDF-1.4\nxref\n0 0\ntrailer\n<<>>\nstartxref\n9\n%%EOF\n";
        assert!(extract_text(bad).is_err());
    }

    // ── Group 5: catch_unwind panic safety ──────────────────────────────────

    /// Non-`PDF` bytes must never panic (`catch_unwind` or not).
    #[test]
    fn non_pdf_bytes_do_not_panic() {
        // If `catch_unwind` were absent and `pdf-extract` panicked, this test
        // would abort the process.  Passing it proves safety for these inputs.
        let _ = extract_text(b"definitely not a pdf");
        let _ = extract_text(b"");
        let _ = extract_text(b"\x00\x01\x02\x03");
    }

    /// A `PDF` whose `Root` entry points to a non-Dictionary object triggers
    /// `get_catalog` to `panic!()` inside `pdf-extract`.  `catch_unwind` must
    /// capture this and return `Err(GraphError::Parse)` instead of aborting.
    #[test]
    fn panic_inducing_pdf_is_caught_not_panicked() {
        let pdf_bytes = build_panic_inducing_pdf();
        let result = extract_text(&pdf_bytes);
        assert!(
            result.is_err(),
            "panic-inducing PDF must return an error, not succeed"
        );
    }

    /// The error from a panic-inducing `PDF` must be the `Parse` variant.
    #[test]
    fn panic_result_is_parse_error_variant() {
        let pdf_bytes = build_panic_inducing_pdf();
        let err = extract_text(&pdf_bytes).unwrap_err();
        assert!(
            matches!(err, GraphError::Parse { .. }),
            "panic must produce Parse error, got: {err:?}"
        );
    }

    /// The `file` field of the `Parse` error from a panic must be `<pdf>`.
    #[test]
    fn panic_result_file_label_is_angle_pdf() {
        let pdf_bytes = build_panic_inducing_pdf();
        let err = extract_text(&pdf_bytes).unwrap_err();
        if let GraphError::Parse { file, .. } = &err {
            assert_eq!(file, "<pdf>", "file label must be '<pdf>'");
        } else {
            panic!("expected Parse error, got {err:?}");
        }
    }

    /// Multiple distinct malformed inputs must all return errors, never panic.
    #[test]
    fn multiple_malformed_inputs_all_return_errors() {
        let inputs: &[&[u8]] = &[
            b"",
            b"\x00",
            b"%PDF-1.4",
            b"%PDF-1.4\n%%EOF",
            b"not a pdf",
            b"\xFF\xFF\xFF\xFF",
            b"PK\x03\x04",
        ];
        for input in inputs {
            assert!(
                extract_text(input).is_err(),
                "expected error for input {input:?}"
            );
        }
    }

    // ── Group 6: Valid PDF tests ──────────────────────────────────────────────

    /// A lopdf-constructed `PDF` with a Helvetica text run must succeed.
    #[test]
    fn valid_pdf_with_simple_text_returns_ok() {
        let pdf = build_pdf_with_text("Hello");
        assert!(
            extract_text(&pdf).is_ok(),
            "valid PDF must extract without error"
        );
    }

    /// Extracted result from a valid `PDF` is a `String`.
    #[test]
    fn valid_pdf_extracted_result_is_string() {
        let pdf = build_pdf_with_text("Test");
        let _text: String = extract_text(&pdf).expect("valid PDF must extract");
    }

    /// Extracted text from a `PDF` containing `Hello World` contains expected chars.
    #[test]
    fn valid_pdf_extracted_text_contains_expected_chars() {
        let pdf = build_pdf_with_text("Hello World");
        let text = extract_text(&pdf).expect("valid PDF with Hello World must extract");
        // `WinAnsiEncoding` + Helvetica must yield at least some of the ASCII chars.
        assert!(
            text.contains('H') || text.contains('e') || text.contains('o'),
            "expected some of 'Hello' in extracted text; got: {text:?}"
        );
    }

    /// A `PDF` with a multi-word phrase must extract successfully.
    #[test]
    fn valid_pdf_with_multiword_text_returns_ok() {
        let pdf = build_pdf_with_text("The quick brown fox");
        assert!(extract_text(&pdf).is_ok());
    }

    /// A `PDF` containing digits must extract successfully.
    #[test]
    fn valid_pdf_with_numeric_text_returns_ok() {
        let pdf = build_pdf_with_text("42 100 0");
        assert!(extract_text(&pdf).is_ok());
    }

    /// A `PDF` with uppercase `ASCII` letters must extract successfully.
    #[test]
    fn valid_pdf_with_uppercase_ascii_returns_ok() {
        let pdf = build_pdf_with_text("ABCDEFGHIJKLMNOPQRSTUVWXYZ");
        assert!(extract_text(&pdf).is_ok());
    }

    /// A two-page `PDF` must extract without error.
    #[test]
    fn two_page_pdf_returns_ok() {
        let pdf = build_two_page_pdf("Page one", "Page two");
        assert!(extract_text(&pdf).is_ok());
    }

    /// A two-page `PDF` must produce at least some text content.
    #[test]
    fn two_page_pdf_produces_non_trivial_output() {
        let pdf = build_two_page_pdf("Alpha", "Beta");
        let text = extract_text(&pdf).expect("two-page PDF must extract");
        assert!(
            !text.trim().is_empty(),
            "two-page PDF must produce some output; got: {text:?}"
        );
    }

    /// Calling `extract_text` twice on the same valid `PDF` bytes must return
    /// identical results (determinism, R4).
    #[test]
    fn repeated_calls_on_valid_pdf_are_deterministic() {
        let pdf = build_pdf_with_text("Determinism");
        let r1 = extract_text(&pdf).expect("first call must succeed");
        let r2 = extract_text(&pdf).expect("second call must succeed");
        assert_eq!(r1, r2, "repeated calls must return identical text");
    }

    /// Calling `extract_text` twice on the same invalid bytes returns the same
    /// error kind each time.
    #[test]
    fn repeated_calls_on_invalid_input_produce_same_error_kind() {
        let input = b"not a pdf at all";
        let e1 = extract_text(input).unwrap_err();
        let e2 = extract_text(input).unwrap_err();
        assert_eq!(
            e1.kind(),
            e2.kind(),
            "repeated error calls must return the same kind"
        );
    }

    /// Two different valid `PDF`s with different text both succeed independently.
    #[test]
    fn two_different_valid_pdfs_extract_independently() {
        let pdf_a = build_pdf_with_text("Alpha");
        let pdf_b = build_pdf_with_text("Omega");
        assert!(extract_text(&pdf_a).is_ok(), "pdf_a must extract");
        assert!(extract_text(&pdf_b).is_ok(), "pdf_b must extract");
    }

    // ── Group 7: Error variant and kind checks ────────────────────────────────

    /// An invalid `PDF` must produce a `Parse` error (not `Io`).
    #[test]
    fn invalid_pdf_error_is_parse_variant() {
        let err = extract_text(b"not a pdf").unwrap_err();
        assert!(
            matches!(err, GraphError::Parse { .. }),
            "invalid bytes must produce Parse error, got {err:?}"
        );
    }

    /// The `kind()` of a `Parse` error must be `"parse"`.
    #[test]
    fn parse_error_kind_tag_is_parse() {
        let err = extract_text(b"garbage").unwrap_err();
        assert_eq!(err.kind(), "parse");
    }

    /// The `kind()` of a size-cap `Io` error must be `"io"`.
    #[test]
    fn io_error_kind_tag_is_io() {
        let big = vec![0_u8; MAX_PDF_BYTES + 1];
        let err = extract_text(&big).unwrap_err();
        assert_eq!(err.kind(), "io");
    }

    /// The `file` field of any `Parse` error from [`extract_text`] must be `<pdf>`.
    #[test]
    fn parse_error_file_field_is_angle_pdf() {
        let err = extract_text(b"not a pdf").unwrap_err();
        if let GraphError::Parse { file, .. } = &err {
            assert_eq!(file, "<pdf>", "file label must be '<pdf>'");
        } else {
            panic!("expected Parse error, got {err:?}");
        }
    }

    /// A valid `PDF` must return `Ok`, not `Err`.
    #[test]
    fn valid_pdf_result_is_ok_not_err() {
        let pdf = build_pdf_with_text("OK");
        assert!(extract_text(&pdf).is_ok());
    }

    // ── Group 8: Constant sanity (compile-time assertions) ───────────────────

    /// The `MAX_PDF_BYTES` constant must be at least 1 `MiB`.
    #[test]
    fn max_pdf_bytes_is_at_least_one_mib() {
        const { assert!(MAX_PDF_BYTES >= 1_048_576) }
    }

    /// The cap must be no more than 256 `MiB`.
    #[test]
    fn max_pdf_bytes_is_at_most_256_mib() {
        const { assert!(MAX_PDF_BYTES <= 268_435_456) }
    }

    /// The cap must be a multiple of 1 `KiB` (clean power-of-two boundary).
    #[test]
    fn max_pdf_bytes_is_multiple_of_1024() {
        assert!(
            MAX_PDF_BYTES.is_multiple_of(1024),
            "cap must be KiB-aligned, got {MAX_PDF_BYTES}"
        );
    }

    // ── Group 9: Additional robustness ───────────────────────────────────────

    /// A `PDF` with only `%%EOF` and no header must error.
    #[test]
    fn eof_marker_only_errors() {
        assert!(extract_text(b"%%EOF\n").is_err());
    }

    /// Binary data that starts with the `PDF` magic but is otherwise random
    /// must error without panicking.
    #[test]
    fn pdf_magic_prefix_then_binary_garbage_errors() {
        let mut bytes = b"%PDF-1.7\n".to_vec();
        bytes.extend((128u8..=200).cycle().take(512));
        assert!(extract_text(&bytes).is_err());
    }

    /// A `CRLF` line-ending variant of the `PDF` header with garbage must error.
    #[test]
    fn pdf_header_crlf_then_garbage_errors() {
        let mut bytes = b"%PDF-1.4\r\n".to_vec();
        bytes.extend_from_slice(b"garbage\r\nmore garbage\r\n%%EOF\r\n");
        assert!(extract_text(&bytes).is_err());
    }

    /// All printable `ASCII` bytes (not a `PDF`) must error.
    #[test]
    fn all_printable_ascii_non_pdf_errors() {
        let printable: Vec<u8> = (32u8..127).collect();
        assert!(extract_text(&printable).is_err());
    }

    /// A large (but within-cap) repeating-pattern buffer must not panic.
    #[test]
    fn large_within_cap_repeating_pattern_no_panic() {
        // 1 MiB of cycling 0..=255, well inside the 64 MiB cap.
        let buf: Vec<u8> = (0u8..=255).cycle().take(1024 * 1024).collect();
        let _ = extract_text(&buf); // must not panic
    }

    /// A `PDF` whose header version string has an unusual high number must error.
    #[test]
    fn pdf_with_unusual_version_and_garbage_errors() {
        let mut bytes = b"%PDF-9.9\n".to_vec();
        bytes.extend_from_slice(&[0u8; 32]);
        assert!(extract_text(&bytes).is_err());
    }

    /// A single-byte buffer must error.
    #[test]
    fn single_byte_buffer_errors() {
        assert!(extract_text(b"X").is_err());
    }

    /// A two-byte buffer must error.
    #[test]
    fn two_byte_buffer_errors() {
        assert!(extract_text(b"AB").is_err());
    }

    /// A valid `PDF` re-parsed produces the same `Ok`/`Err` shape each time
    /// (idempotency).
    #[test]
    fn valid_pdf_bytes_stable_on_reparse() {
        let pdf = build_pdf_with_text("Idempotent");
        let r1 = extract_text(&pdf);
        let r2 = extract_text(&pdf);
        assert_eq!(
            r1.is_ok(),
            r2.is_ok(),
            "Ok/Err status must be stable across reparsings"
        );
    }
}
