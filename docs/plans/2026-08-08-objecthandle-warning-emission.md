# ObjectHandle Warning Emission Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Port qpdf's object-level warning emission so an `ObjectHandle` reports a type or damage warning through the document that owns it, without the caller holding a `&mut Pdf`.

**Architecture:** `DocumentResolver` gains a warn receiver equivalent to `QPDF::warn`. `ObjectHandle` gains `type_warning` / `warn_if_possible` / `object_warning`, each reaching the document through the `Weak<dyn DocumentResolver>` it already carries — qpdf's `QPDF*` context. The no-context branches are ported separately because qpdf's are not the same: `typeWarning` / `objectWarning` throw, `warnIfPossible` writes to the default logger and returns normally. Finally the `try_*` accessors whose qpdf counterparts warn are converted, and the two integer accessors the acceptance criteria need are added.

**Tech Stack:** Rust, `crates/flpdf`. Written when the branch base was `feature/flpdf-qynx.4-cli-output-routing` (PR #677 tip), because `QPDFLogger` and the `Result`-returning warning sink lived there and not on `main` at the time. PR #677's stack (#669, #672, #677) has since merged, so a fresh checkout of this plan targets `main` directly — see Task 6's coverage step.

**Beads issue:** flpdf-25kg.3.27

---

## Background the implementer needs

### The three qpdf emitters differ in their no-context behavior

Resolve the pinned 11.9.0 worktree with the repository script; do not hard-code a
home directory:

```bash
qpdf_source=$(scripts/fetch-qpdf-source.sh --print-path)
```

Every `libqpdf/...` and `include/qpdf/...` path below is relative to that root.

```cpp
// libqpdf/QPDFObjectHandle.cc:2168-2189
void QPDFObjectHandle::typeWarning(char const* expected_type, std::string const& warning) {
    QPDF* context = nullptr;
    std::string description;
    if (!dereference()) {
        throw std::logic_error("attempted to dereference an uninitialized QPDFObjectHandle");
    }
    this->obj->getDescription(context, description);
    warn(context, QPDFExc(qpdf_e_object, "", description, 0,
        std::string("operation for ") + expected_type + " attempted on object of type " +
            getTypeName() + ": " + warning));
}

// :2191-2201
void QPDFObjectHandle::warnIfPossible(std::string const& warning) {
    QPDF* context = nullptr;
    std::string description;
    if (dereference() && obj->getDescription(context, description)) {
        warn(context, QPDFExc(qpdf_e_damaged_pdf, "", description, 0, warning));
    } else {
        *QPDFLogger::defaultLogger()->getError() << warning << "\n";
    }
}

// :2203-2212
void QPDFObjectHandle::objectWarning(std::string const& warning) {
    QPDF* context = nullptr;
    std::string description;
    this->obj->getDescription(context, description);   // note: no dereference
    warn(context, QPDFExc(qpdf_e_object, "", description, 0, warning));
}

// :2385-2396
void QPDFObjectHandle::warn(QPDF* qpdf, QPDFExc const& e) {
    if (qpdf) { qpdf->warn(e); } else { throw e; }
}
```

`getDescription` is the two-argument form in `libqpdf/qpdf/QPDFObject_private.hh:94-100`:

```cpp
bool getDescription(QPDF*& qpdf, std::string& description) {
    qpdf = value->qpdf;
    description = value->getDescription();
    return qpdf != nullptr;
}
```

So its `bool` result *is* "has a context". That makes `warnIfPossible`'s else-branch exactly the no-context case, and it logs rather than throwing. `typeWarning` and `objectWarning` route through `warn`, which throws. **Two no-context behaviors, not one.** The issue's acceptance criterion 3 states only the throwing one; implement both and treat the criterion as covering `typeWarning` / `objectWarning`.

### `throw e` maps to `Error::System`, not `Error::Internal`

`class QPDFExc: public std::runtime_error` (`include/qpdf/QPDFExc.hh:29`). `crates/flpdf/src/error.rs:19-20` documents `Error::Internal` ↔ `std::logic_error` and `Error::System` ↔ `std::runtime_error`. So:

| qpdf throw site | flpdf error |
|---|---|
| `typeWarning`'s `!dereference()` → `std::logic_error` | `Error::Internal` (already what `try_dereference` returns) |
| `warn(nullptr, e)` → `throw e` (a `QPDFExc`) | `Error::System` |

`Error::System`'s `Display` is `{0}`, so the rendered text is exactly qpdf's `e.what()` for an exception whose filename, object, and offset slots are all empty (see next section).

### The emitted warning must carry **no** location

`QPDF::resolve` reads objects with an empty description argument:

```cpp
// libqpdf/QPDF.cc:1725
QPDFObjectHandle oh = readObjectAtOffset(true, offset, "", og, a_og, false);
```

and `setLastObjectDescription` (`:1297-1309`) therefore produces `"object N G"` with no filename. `typeWarning` then builds `QPDFExc(qpdf_e_object, "" /*filename*/, description, 0 /*offset*/, message)`, so `QPDFExc::createWhat` (`libqpdf/QPDFExc.cc:19-49`) renders `"object N G: <message>"`. **qpdf emits no filename for these warnings.**

On this branch, `ResolverHandle::route_warning` (`crates/flpdf/src/reader/resolver.rs:693-716`) builds its location from `core.description`, which is the *input-source* description — the slot qpdf fills only for damaged-PDF warnings via `QPDFParser::warn` (`libqpdf/QPDFParser.cc:512`, `input->getName()`). Routing handle warnings through the existing `push_warning` would therefore emit a filename qpdf never emits.

So handle warnings need a sink that passes an **empty** location. `route_warning` already takes `description` as a parameter, so this uses its existing seam and changes nothing about PR #672's design. The `"object N G"` half arrives with flpdf-25kg.3.28, which is this issue's declared non-goal.

### Which handles have a context, and why that is not a gap

In qpdf, `QPDFParser` stamps the owning `QPDF*` on every value it creates —
`obj->setDescription(context, description, ...)` (`libqpdf/QPDFParser.cc:416,425,434,442`),
which sets `QPDFValue::qpdf` (`libqpdf/qpdf/QPDFValue.hh:60-66`). So a direct object
parsed from a file has a context upstream. In flpdf only canonical indirect handles
carry `resolver: Some(..)`; direct children built during resolution carry `None`, so
they would take a branch qpdf cannot take.

**`try_get_key` and `try_get_keys` have live production consumers**, so this is not a
documentation-only gap. A leftover `#[allow(dead_code)]` attribute does not prove an
accessor is unused — audit the callers, not the attribute:

| Caller | Path |
| --- | --- |
| `inspect_stream_encryption`, reached from `pipe_stream_data` (`reader/resolver.rs:1015`) | `reader/resolver.rs:1829,1844,1848,1854,1874` |
| `decode_params_from_consuming_handle` | `stream_filter.rs:584,588` |
| `encode_stream_data_from_handle` / `decode_stream_data_from_handle_with_mode` | `filters.rs:350,351,482,483` |
| `try_is_dictionary_of_type` — guarded by an `is_dictionary` test first, mirroring qpdf's `isDictionary() &&` (`libqpdf/QPDFObjectHandle.cc:462-466`) | `object_handle.rs:1144,1151` |

That is why Task 4 defers the two dictionary warning arms; see its own section.
`try_get_int_value` / `try_get_int_value_as_int` are the accessors with no production
caller, which is what makes them safe to land warning and all.

### `try_as_dictionary` must stay silent

The issue description says qpdf emits `typeWarning` where `try_as_dictionary` returns `None`. It does not. `asDictionary()` is the silent internal helper; the warning sites are `getKey` (`:983-988`, `"returning null for attempted key retrieval"`), `getKeys` (`:999-1003`, `"treating as empty"`), and `getDictAsMap`. Making `try_as_dictionary` warn would add a warning qpdf never emits. `try_get_keys` currently delegates through `try_as_dictionary`, so the emit has to move into the consumers.

---

## Task 1: A warn receiver on `DocumentResolver`

**Files:**
- Modify: `crates/flpdf/src/object_handle.rs:71-99` (the `DocumentResolver` trait)
- Modify: `crates/flpdf/src/reader/resolver.rs:1854` (`impl DocumentResolver for ResolverHandle<R>`)
- Modify: `crates/flpdf/src/reader/resolver.rs` (new `push_object_warning`, beside `push_warning_with_offset` at `:674-691`)
- Test: `crates/flpdf/src/reader/resolver.rs` (existing `#[cfg(test)] mod` at the end of the file)

**Step 1: Write the failing test**

Add to the test module at the end of `crates/flpdf/src/reader/resolver.rs`. Model the logger capture on the existing `warning_location_omits_empty_description_and_zero_offset_like_qpdf` test (`:2310`) — reuse whatever sink helper it uses rather than inventing one.

```rust
#[test]
fn an_object_warning_carries_no_location_even_when_the_document_has_a_description() {
    // qpdf's typeWarning builds QPDFExc(qpdf_e_object, "" /*filename*/, description, 0, msg)
    // (libqpdf/QPDFObjectHandle.cc:2180-2188), so createWhat (libqpdf/QPDFExc.cc:19-49)
    // interposes nothing when the object description is empty. The input-source
    // description belongs to damagedPDF warnings, not to these.
    let (pdf, sink) = pdf_with_captured_warnings_and_description("damaged.pdf");
    pdf.resolver.push_object_warning("operation for dictionary attempted on object of type integer: treating as empty").unwrap();
    assert_eq!(
        sink.take_utf8(),
        "WARNING: operation for dictionary attempted on object of type integer: treating as empty\n"
    );
}

#[test]
fn an_object_warning_is_collected_alongside_document_warnings_in_emission_order() {
    let (pdf, _sink) = pdf_with_captured_warnings_and_description("damaged.pdf");
    pdf.resolver.push_warning("first").unwrap();
    pdf.resolver.push_object_warning("second").unwrap();
    let messages: Vec<_> = pdf
        .resolver
        .repair_diagnostics()
        .entries()
        .iter()
        .map(|entry| entry.message.clone())
        .collect();
    assert_eq!(messages, vec!["first".to_owned(), "second".to_owned()]);
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p flpdf --lib an_object_warning_ -- --nocapture`
Expected: FAIL — `no method named push_object_warning`.

**Step 3: Write minimal implementation**

In `crates/flpdf/src/reader/resolver.rs`, beside `push_warning_with_offset`:

```rust
    /// [`Self::push_warning`] for a warning an object raised about itself.
    ///
    /// qpdf's object-level emitters build their exception with an empty
    /// filename — `QPDFExc(qpdf_e_object, "", description, 0, message)`
    /// (`libqpdf/QPDFObjectHandle.cc:2180-2188`) — and the object description
    /// is the only location `QPDFExc::createWhat` (`libqpdf/QPDFExc.cc:19-49`)
    /// can interpose. `QPDF::resolve` reads with an empty description
    /// (`libqpdf/QPDF.cc:1725`), so that slot holds `"object N G"` and never a
    /// file name. [`Self::push_warning`]'s input-source description is the
    /// slot qpdf fills for `damagedPDF` warnings instead
    /// (`libqpdf/QPDFParser.cc:512`), which is why this route passes an empty
    /// location rather than reusing it. Object descriptions themselves arrive
    /// with the description propagation work; until then this is bare.
    ///
    /// Same borrow discipline as [`Self::push_warning`]: the `borrow_mut()`
    /// is taken and dropped before the logger write.
    pub(crate) fn push_object_warning(&self, message: impl Into<String>) -> Result<()> {
        let message = message.into();
        let (logger, suppress_warnings) = {
            let mut core = self.core.borrow_mut();
            core.repair_diagnostics
                .push(Diagnostic::warning(message.clone(), None));
            (core.logger.clone(), core.suppress_warnings)
        };
        route_warning(&logger, suppress_warnings, "", None, &message)
    }
```

Then add the trait method in `crates/flpdf/src/object_handle.rs`:

```rust
    /// The document-side half of qpdf's `QPDFObjectHandle::warn`
    /// (`libqpdf/QPDFObjectHandle.cc:2385-2396`): `QPDF::warn`
    /// (`libqpdf/QPDF.cc:487-494`) reached from an object rather than from a
    /// caller holding the document. The message is already fully formed, as
    /// qpdf's `QPDFExc` is by the time it reaches `QPDF::warn`.
    fn warn(&self, message: String) -> Result<()>;
```

and implement it on `ResolverHandle<R>` (`crates/flpdf/src/reader/resolver.rs:1854`):

```rust
    fn warn(&self, message: String) -> Result<()> {
        self.push_object_warning(message)
    }
```

Every other `impl DocumentResolver` in the tree is a test double (`object_handle.rs:3785,3802,3884,3897,3918,4750,4781`; `resolver.rs:2281`). Because `warn` has no sensible default, give each a body. For the doubles that do not assert on warnings, `Ok(())` is right; give `RecordingResolver` and `LoggedErrorResolver` a recording body if their existing shape makes that natural.

**Step 4: Run test to verify it passes**

Run: `cargo test -p flpdf --lib an_object_warning_`
Expected: PASS, 2 tests.

**Step 5: Commit**

```bash
git add crates/flpdf/src/object_handle.rs crates/flpdf/src/reader/resolver.rs
git commit -m "feat(object): add a document warn receiver reachable from a handle"
```

---

## Task 2: `ObjectHandle::type_warning`

**Files:**
- Modify: `crates/flpdf/src/object_handle.rs` (beside `try_dereference` at `:856`)
- Test: `crates/flpdf/src/object_handle.rs` (new `#[cfg(test)] mod warning_emission_tests`)

**Step 1: Write the failing test**

```rust
#[cfg(test)]
mod warning_emission_tests {
    use super::*;

    #[derive(Default)]
    struct WarningRecorder {
        warnings: RefCell<Vec<String>>,
    }

    impl DocumentResolver for WarningRecorder {
        fn resolve_indirect(&self, _object_ref: ObjectRef, handle: &ObjectHandle) -> crate::Result<()> {
            handle.set_resolved(ObjectValue::Integer(7));
            Ok(())
        }

        fn warn(&self, message: String) -> crate::Result<()> {
            self.warnings.borrow_mut().push(message);
            Ok(())
        }
    }

    fn recorder() -> (Rc<dyn DocumentResolver>, Rc<WarningRecorder>) {
        let recorder = Rc::new(WarningRecorder::default());
        (recorder.clone(), recorder)
    }

    #[test]
    fn type_warning_through_a_context_matches_qpdf_message_text() {
        // libqpdf/QPDFObjectHandle.cc:2180-2188
        let (resolver, recorder) = recorder();
        let handle = ObjectHandle::new_indirect_with_resolver(
            ObjectRef { number: 3, generation: 0 },
            Rc::downgrade(&resolver),
        );
        handle.type_warning("dictionary", "treating as empty").unwrap();
        assert_eq!(
            recorder.warnings.borrow().as_slice(),
            ["operation for dictionary attempted on object of type integer: treating as empty"]
        );
    }

    #[test]
    fn type_warning_without_a_context_returns_the_error_qpdf_throws() {
        // warn(nullptr, e) throws the QPDFExc itself
        // (libqpdf/QPDFObjectHandle.cc:2393-2395); QPDFExc derives from
        // std::runtime_error (include/qpdf/QPDFExc.hh:29), which this crate
        // classifies as Error::System.
        let handle = ObjectHandle::integer(7);
        let error = handle.type_warning("dictionary", "treating as empty").unwrap_err();
        assert!(matches!(
            error,
            crate::Error::System(ref message)
                if message == "operation for dictionary attempted on object of type integer: treating as empty"
        ));
    }

    #[test]
    fn two_warnings_from_one_handle_reach_the_sink_in_emission_order() {
        let (resolver, recorder) = recorder();
        let handle = ObjectHandle::new_indirect_with_resolver(
            ObjectRef { number: 3, generation: 0 },
            Rc::downgrade(&resolver),
        );
        handle.type_warning("dictionary", "treating as empty").unwrap();
        handle.type_warning("array", "treating as empty").unwrap();
        assert_eq!(
            recorder.warnings.borrow().as_slice(),
            [
                "operation for dictionary attempted on object of type integer: treating as empty",
                "operation for array attempted on object of type integer: treating as empty",
            ]
        );
    }
}
```

`ObjectRef`'s literal form may differ; copy the construction the neighbouring tests already use.

**Step 2: Run test to verify it fails**

Run: `cargo test -p flpdf --lib warning_emission_tests`
Expected: FAIL — `no method named type_warning`.

**Step 3: Write minimal implementation**

Beside `try_dereference`:

```rust
    /// This handle's owning document, qpdf's `QPDF* context`.
    ///
    /// `QPDFValue::qpdf` is set by the description machinery
    /// (`libqpdf/qpdf/QPDFValue.hh:60-83`), so upstream a direct object
    /// parsed from a file carries a context too. Here only a canonical
    /// indirect slot does; direct children take the no-context branch, which
    /// is qpdf's behavior for an object built without a document. The rest
    /// arrives with object description propagation.
    fn context(&self) -> Option<Rc<dyn DocumentResolver>> {
        self.0.borrow().resolver.as_ref().and_then(Weak::upgrade)
    }

    /// Emit `message` through this handle's context, or report it as the
    /// error qpdf throws when there is none.
    ///
    /// Ports `QPDFObjectHandle::warn`
    /// (`libqpdf/QPDFObjectHandle.cc:2385-2396`). `QPDFExc` derives from
    /// `std::runtime_error` (`include/qpdf/QPDFExc.hh:29`), so its `throw`
    /// arm is [`crate::Error::System`]; with an empty filename, object
    /// description, and offset, `QPDFExc::createWhat`
    /// (`libqpdf/QPDFExc.cc:19-49`) renders `what()` as the bare message,
    /// which is what `Error::System`'s `Display` produces.
    fn warn_through_context(&self, message: String) -> Result<()> {
        match self.context() {
            Some(context) => context.warn(message),
            None => Err(Error::System(message)),
        }
    }

    /// Report that an accessor for `expected_type` ran on this handle.
    ///
    /// Ports `QPDFObjectHandle::typeWarning`
    /// (`libqpdf/QPDFObjectHandle.cc:2168-2189`). qpdf's `!dereference()`
    /// arm throws `std::logic_error`; here [`Self::try_dereference`] already
    /// returns [`crate::Error::Internal`], this crate's counterpart, for the
    /// one state that cannot resolve.
    pub(crate) fn type_warning(&self, expected_type: &str, warning: &str) -> Result<()> {
        self.try_dereference()?;
        self.warn_through_context(format!(
            "operation for {expected_type} attempted on object of type {}: {warning}",
            self.type_name()
        ))
    }
```

**Step 4: Run test to verify it passes**

Run: `cargo test -p flpdf --lib warning_emission_tests`
Expected: PASS, 3 tests.

**Step 5: Commit**

```bash
git add crates/flpdf/src/object_handle.rs
git commit -m "feat(object): port QPDFObjectHandle::typeWarning"
```

---

## Task 3: `ObjectHandle::warn_if_possible` and `object_warning`

**Files:**
- Modify: `crates/flpdf/src/object_handle.rs` (beside `type_warning`)
- Test: `crates/flpdf/src/object_handle.rs` (`mod warning_emission_tests`)

**Step 1: Write the failing test**

```rust
    #[test]
    fn warn_if_possible_through_a_context_reaches_the_same_sink() {
        let (resolver, recorder) = recorder();
        let handle = ObjectHandle::new_indirect_with_resolver(
            ObjectRef { number: 3, generation: 0 },
            Rc::downgrade(&resolver),
        );
        handle.warn_if_possible("requested value of integer is too big; returning INT_MAX").unwrap();
        assert_eq!(
            recorder.warnings.borrow().as_slice(),
            ["requested value of integer is too big; returning INT_MAX"]
        );
    }

    #[test]
    fn warn_if_possible_without_a_context_logs_instead_of_failing() {
        // libqpdf/QPDFObjectHandle.cc:2196-2200 — the else-branch writes the
        // bare message to QPDFLogger::defaultLogger()->getError() and returns
        // normally. Unlike typeWarning, it does not throw.
        let logger = crate::QPDFLogger::create();
        let sink = /* capture handle, as pdf_logger_tests does */;
        logger.set_error(Some(sink.pipeline()));
        let handle = ObjectHandle::integer(7);
        handle.warn_if_possible_to("requested value of integer is too big; returning INT_MAX", &logger).unwrap();
        assert_eq!(sink.take_utf8(), "requested value of integer is too big; returning INT_MAX\n");
    }

    #[test]
    fn object_warning_passes_its_message_through_unchanged() {
        // libqpdf/QPDFObjectHandle.cc:2203-2212 — no type name is interposed
        // and, unlike typeWarning, no dereference is performed.
        let (resolver, recorder) = recorder();
        let handle = ObjectHandle::new_indirect_with_resolver(
            ObjectRef { number: 3, generation: 0 },
            Rc::downgrade(&resolver),
        );
        handle.object_warning("unresolved name object").unwrap();
        assert_eq!(recorder.warnings.borrow().as_slice(), ["unresolved name object"]);
        assert!(!handle.is_resolved(), "objectWarning does not dereference");
    }

    #[test]
    fn object_warning_without_a_context_returns_the_error_qpdf_throws() {
        let handle = ObjectHandle::integer(7);
        let error = handle.object_warning("unresolved name object").unwrap_err();
        assert!(matches!(error, crate::Error::System(ref message) if message == "unresolved name object"));
    }
```

**Note for the implementer — resolve this before writing the code.** The default-logger test above is written against a seam (`warn_if_possible_to`) that does not exist yet. `QPDFLogger::default_logger()` (`crates/flpdf/src/logger.rs:104`) is process-global, so a test that redirects it is order-dependent against every other test in the binary. Pick one and say why in the doc comment:

- **(a)** `warn_if_possible` calls `crate::QPDFLogger::default_logger().error(..)` directly — verbatim qpdf — and the test asserts through whatever isolation `pdf_logger_tests.rs` already established for the default logger. Check that file first; if it has a pattern for this, use it and delete `warn_if_possible_to`.
- **(b)** If it does not, keep `warn_if_possible` verbatim and cover the branch through a private `warn_if_possible_with_logger(&self, warning, logger)` that `warn_if_possible` calls with the default. That is an added seam, so it needs a one-line note in the doc comment saying it exists only to make the branch testable.

Do not leave the branch uncovered — `flpdf` changed lines must be 100% covered.

**Step 2: Run test to verify it fails**

Run: `cargo test -p flpdf --lib warning_emission_tests`
Expected: FAIL — methods not defined.

**Step 3: Write minimal implementation**

```rust
    /// Report damage this handle noticed about itself.
    ///
    /// Ports `QPDFObjectHandle::warnIfPossible`
    /// (`libqpdf/QPDFObjectHandle.cc:2191-2201`). Its condition is
    /// `dereference() && obj->getDescription(context, description)`, and that
    /// second call returns `qpdf != nullptr`
    /// (`libqpdf/qpdf/QPDFObject_private.hh:94-100`) — so the else-branch is
    /// exactly the no-context case. Unlike [`Self::type_warning`] it writes
    /// the bare message to the default logger and returns normally rather
    /// than reporting an error.
    pub(crate) fn warn_if_possible(&self, warning: &str) -> Result<()> {
        // The context is tested BEFORE resolution. A handle whose document
        // has been dropped is this port's counterpart of qpdf's null context,
        // and it is also the one state `try_dereference` cannot resolve —
        // dereferencing first would turn it into an error and lose the
        // branch, while still swallowing a reachable document's genuine
        // resolution failure.
        let Some(context) = self.context() else {
            return crate::QPDFLogger::default_logger().error(format!("{warning}\n"));
        };
        self.try_dereference()?;
        context.warn(warning.to_owned())
    }

    /// Report an object-level problem whose message qpdf passes through
    /// unchanged.
    ///
    /// Ports `QPDFObjectHandle::objectWarning`
    /// (`libqpdf/QPDFObjectHandle.cc:2203-2212`). No type name is interposed,
    /// and — unlike [`Self::type_warning`] — qpdf performs no dereference
    /// here, because its callers have already type-checked.
    pub(crate) fn object_warning(&self, warning: &str) -> Result<()> {
        self.warn_through_context(warning.to_owned())
    }
```

`try_dereference` in `warn_if_possible` needs care: it returns `Err` for a handle whose document was dropped, which is flpdf's closest analogue of qpdf's "no context". Route that to the logger branch rather than propagating, and say so in the doc comment. Confirm the exact shape against `try_dereference` (`:856-874`) before writing.

**Step 4: Run test to verify it passes**

Run: `cargo test -p flpdf --lib warning_emission_tests`
Expected: PASS, 7 tests.

**Step 5: Commit**

```bash
git add crates/flpdf/src/object_handle.rs
git commit -m "feat(object): port warnIfPossible and objectWarning"
```

---
## Task 4: Leave the dictionary accessors silent — do NOT add the warning arms

**Files:**
- Modify: `crates/flpdf/src/object_handle.rs` (`try_get_keys`, `try_get_key`) — doc comments only
- Test: `crates/flpdf/src/object_handle.rs` (`mod warning_emission_tests`)

**This task deliberately does not port the warnings, and an earlier revision of this
plan was wrong to instruct otherwise.** qpdf does emit them —
`typeWarning("dictionary", "treating as empty")` at
`libqpdf/QPDFObjectHandle.cc:1000` and
`typeWarning("dictionary", "returning null for attempted key retrieval")` at `:984` —
but its receiver always has a context, because `QPDFParser` stamps the owning `QPDF*`
on every value it creates (`libqpdf/QPDFParser.cc:416-442`). flpdf's direct children
have none, so the emit would reach `warn`'s `throw` arm on a path qpdf warns and
continues on.

That path is live. `SF_FlateLzwDecode::setDecodeParms` calls `decode_parms.getKeys()`
with no type guard (`libqpdf/SF_FlateLzwDecode.cc:28`); the flpdf counterpart is
`stream_filter.rs`'s `decode_params_from_consuming_handle`. Adding the arms turns the
corpus row "present non-dictionary /DecodeParms" into
`Error::System("operation for dictionary attempted on object of type integer: treating
as empty")` where the legacy path decodes fine — caught by
`filters::tests::equivalence::legacy_and_native_entry_points_agree_on_every_corpus_row`
and two `stream_filter` tests. `inspect_stream_encryption`
(`reader/resolver.rs:1829-1874`, reached from `pipe_stream_data`) is a second live
consumer.

The prerequisite is parse-time context stamping on direct children, which belongs to
the canonical resolver work, **not** to object description propagation — upstream both
live in the same `QPDFValue` fields, but the context pointer is what these accessors
need and the description string is separate. Tracked as its own issue; see the beads
tracker for the follow-up that depends on it.

`try_as_dictionary` stays silent for a different and permanent reason: it ports
`asDictionary()`, the internal helper, which does not warn in qpdf either.

**Step 1: Write the tests that pin the silence**

```rust
    #[test]
    fn get_key_on_a_non_dictionary_returns_null_without_warning_yet() {
        let (handle, recorder) = handle_resolving(ObjectValue::Integer(7));

        assert!(handle.try_get_key(b"Type").unwrap().is_null());
        assert!(handle.try_get_keys().unwrap().is_empty());

        assert!(warnings(&recorder).is_empty());
    }

    #[test]
    fn as_dictionary_on_a_non_dictionary_stays_silent_like_qpdf() {
        let (handle, recorder) = handle_resolving(ObjectValue::Integer(7));

        assert!(handle.try_as_dictionary().unwrap().is_none());

        assert!(warnings(&recorder).is_empty());
    }

    #[test]
    fn dictionary_accessors_neither_warn_nor_change_their_result() {
        let (handle, recorder) = handle_resolving(ObjectValue::Dictionary(
            [
                (b"A".to_vec(), ObjectHandle::integer(1)),
                (b"B".to_vec(), ObjectHandle::null()),
            ]
            .into_iter()
            .collect(),
        ));

        assert_eq!(
            handle.try_get_keys().unwrap(),
            [b"A".to_vec()].into_iter().collect::<BTreeSet<_>>()
        );
        assert_eq!(handle.try_get_key(b"A").unwrap().as_integer(), Some(1));
        assert!(handle.try_get_key(b"Missing").unwrap().is_null());
        assert!(warnings(&recorder).is_empty());
    }
```

The first test is the one that matters: it makes the silence deliberate, so whoever
lands the context prerequisite has to change a test on purpose rather than discover
the behavior by accident.

**Step 2: Run them**

Run: `cargo test -p flpdf --lib warning_emission_tests`
Expected: PASS. They pass against the accessors as they already are; nothing about
their behavior changes in this task.

**Step 3: Record the reason in the doc comments**

Extend both accessors' doc comments to say the warning is qpdf's and is not
reproduced yet, and why — the receiver must be able to reach its owning document.
Keep the citation to `:984` / `:1000` so the gap is findable from the code.

While here, make `try_get_key` fetch through a single `with_value` that both
type-tests and fetches, rather than `as_dictionary().is_none()` followed by
`get_key`. `as_dictionary()` clones the whole entry map (`entries.clone()`), which is
review-pattern 1 in `.claude/rules/pdf-rust-review-patterns.md`. This is an
independent improvement and carries no behavior change:

```rust
    pub(crate) fn try_get_key(&self, key: &[u8]) -> Result<ObjectHandle> {
        self.try_dereference()?;
        Ok(self
            .with_value(|value| match value {
                Some(ObjectValue::Dictionary(entries)) => entries.get(key).cloned(),
                _ => None,
            })
            .unwrap_or_else(ObjectHandle::null))
    }
```

**Step 4: Confirm nothing regressed**

Run: `cargo test -p flpdf`
Expected: PASS, including the three consumer tests named above, **unedited**. If any
of them needed editing, the warning arms leaked back in.

**Step 5: Commit**

```bash
git add crates/flpdf/src/object_handle.rs
git commit -m "test(object): pin the silent dictionary accessors and their reason"
```

## Task 5: The integer accessors

**Files:**
- Modify: `crates/flpdf/src/object_handle.rs` (beside `try_as_integer` at `:1060-1066`)
- Test: `crates/flpdf/src/object_handle.rs` (`mod warning_emission_tests`)

These do not exist yet; the acceptance criteria's INT_MIN / INT_MAX failure paths need them, and they are the only `warn_if_possible` consumers.

**Step 1: Write the failing test**

```rust
    #[test]
    fn get_int_value_on_a_non_integer_warns_and_returns_zero() {
        // libqpdf/QPDFObjectHandle.cc:503-513
        let (resolver, recorder) = recorder_resolving(ObjectValue::Name(b"Foo".to_vec()));
        let handle = /* indirect handle on that resolver */;
        assert_eq!(handle.try_get_int_value().unwrap(), 0);
        assert_eq!(
            recorder.warnings.borrow().as_slice(),
            ["operation for integer attempted on object of type name: returning 0"]
        );
    }

    #[test]
    fn an_integer_below_int_min_saturates_and_warns() {
        // libqpdf/QPDFObjectHandle.cc:526-534
        let (resolver, recorder) = recorder_resolving(ObjectValue::Integer(i64::from(i32::MIN) - 1));
        let handle = /* indirect handle on that resolver */;
        assert_eq!(handle.try_get_int_value_as_int().unwrap(), i32::MIN);
        assert_eq!(
            recorder.warnings.borrow().as_slice(),
            ["requested value of integer is too small; returning INT_MIN"]
        );
    }

    #[test]
    fn an_integer_above_int_max_saturates_and_warns() {
        let (resolver, recorder) = recorder_resolving(ObjectValue::Integer(i64::from(i32::MAX) + 1));
        let handle = /* indirect handle on that resolver */;
        assert_eq!(handle.try_get_int_value_as_int().unwrap(), i32::MAX);
        assert_eq!(
            recorder.warnings.borrow().as_slice(),
            ["requested value of integer is too big; returning INT_MAX"]
        );
    }

    #[test]
    fn an_in_range_integer_neither_saturates_nor_warns() {
        let (resolver, recorder) = recorder_resolving(ObjectValue::Integer(7));
        let handle = /* indirect handle on that resolver */;
        assert_eq!(handle.try_get_int_value_as_int().unwrap(), 7);
        assert!(recorder.warnings.borrow().is_empty());
    }
```

Generalize `WarningRecorder` from Task 2 into `recorder_resolving(value)` so each test can pick the resolved payload. Boundary values `i32::MIN` and `i32::MAX` exactly must be in the in-range test too — qpdf's comparisons are strict (`v < INT_MIN`, `v > INT_MAX`), so the endpoints do not warn.

**Step 2: Run test to verify it fails**

Run: `cargo test -p flpdf --lib warning_emission_tests`
Expected: FAIL — methods not defined.

**Step 3: Write minimal implementation**

```rust
    /// This handle's integer value, warning and yielding `0` for any other
    /// type.
    ///
    /// Ports `QPDFObjectHandle::getIntValue`
    /// (`libqpdf/QPDFObjectHandle.cc:503-513`).
    pub(crate) fn try_get_int_value(&self) -> Result<i64> {
        self.try_dereference()?;
        match self.as_integer() {
            Some(value) => Ok(value),
            None => {
                self.type_warning("integer", "returning 0")?;
                Ok(0)
            }
        }
    }

    /// [`Self::try_get_int_value`] saturated into `i32`, warning at each
    /// clamp.
    ///
    /// Ports `QPDFObjectHandle::getIntValueAsInt`
    /// (`libqpdf/QPDFObjectHandle.cc:526-543`). The comparisons are strict,
    /// so `INT_MIN` and `INT_MAX` themselves pass through unwarned.
    pub(crate) fn try_get_int_value_as_int(&self) -> Result<i32> {
        let value = self.try_get_int_value()?;
        if value < i64::from(i32::MIN) {
            self.warn_if_possible("requested value of integer is too small; returning INT_MIN")?;
            Ok(i32::MIN)
        } else if value > i64::from(i32::MAX) {
            self.warn_if_possible("requested value of integer is too big; returning INT_MAX")?;
            Ok(i32::MAX)
        } else {
            Ok(value as i32)
        }
    }
```

The message text matches `outline_document_helper.rs:512-528`, which already carries qpdf's exact wording. Do not migrate that helper — it is a declared non-goal.

**Step 4: Run test to verify it passes**

Run: `cargo test -p flpdf --lib warning_emission_tests`
Expected: PASS.

**Step 5: Commit**

```bash
git add crates/flpdf/src/object_handle.rs
git commit -m "feat(object): port getIntValue and getIntValueAsInt"
```

---

## Task 6: Correspondence record and quality gates

**Files:**
- Modify: `docs/qpdf-correspondence.md` (the `QPDFObjectHandle.cc` row)

**Step 1: Record the ported surface**

Add `typeWarning` / `warnIfPossible` / `objectWarning` / `warn` / `getIntValue` / `getIntValueAsInt` to the `QPDFObjectHandle.cc` row — **not** `getKey` / `getKeys`, which Task 4 leaves unported — and note the two deviations this issue knowingly carries:

1. Object descriptions are empty, so warnings render without qpdf's `"object N G: "` prefix until the description propagation work lands.
2. Only canonical indirect handles carry a context, so direct children take the no-context branch. Record that this is what defers `getKey` / `getKeys`, and name the live consumers (`reader/resolver.rs`'s `inspect_stream_encryption`, `stream_filter.rs`'s consuming `/DecodeParms` read) so the row states a reachable constraint rather than a theoretical one.

**Step 2: Run the workspace gates**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
cargo test -p flpdf --doc
```

Expected: all green. `cargo fmt` before pushing is a hard CI gate here.

**Step 3: Changed-line coverage**

Commit first — the script errors on a dirty tree by design.

```bash
scripts/patch-coverage.sh --base main
```

Expected: `flpdf` changed lines 100%. The script's fresh mode runs
`--features qpdf-zlib-compat --ignore-run-fail`, matching the CI Coverage job:
`overlay::byte_gate`'s byte-identical tests are gated behind that feature, so a
measurement without it reports hundreds of false-positive uncovered lines.

The whole qynx.4 stack (#669, #672, #677) has since merged to `main`, so `main`
is this branch's actual fork point — use it rather than a feature-branch name,
which is deleted once its PR merges and leaves `--base` unresolvable
(`scripts/patch-coverage.sh` calls `git merge-base` on it and exits 2).

**Step 4: Byte-identical corpus**

The existing corpus must be unaffected. `cargo test`'s trailing `[TESTNAME]` is a
substring filter, so a package/filter pair that matches nothing compiles and reports
success while running zero tests — name the targets explicitly instead. The
authoritative list is the `qpdf-zlib-compat` block in `.github/workflows/ci.yml`;
these gated suites are not run by a plain `cargo test`.

```bash
for t in zlib_compat_tests cmp_diff_zero_tests cmp_null_visibility_tests \
         deterministic_id_qpdf_parity_tests cmp_generate_objstm_tests \
         cmp_linearize_tests cmp_linearize_objstm_tests; do
  cargo test -p flpdf --features qpdf-zlib-compat --test "$t"
done
cargo test -p flpdf --features qpdf-zlib-compat --lib overlay::byte_gate
for t in cli_byte_identical cli_byte_identical_overlay encrypt_cli_tests \
         compat_baseline_static_id compat_matrix_baseline; do
  cargo test -p flpdf-cli --features qpdf-zlib-compat --test "$t"
done
# flpdf-qtest-tools' flate-compression-tolerance e2e is gated behind the same
# feature (`.github/workflows/ci.yml:289-292`); a plain `cargo test` cfgs it out.
cargo test -p flpdf-qtest-tools --features qpdf-zlib-compat --test e2e
```

Check each line's `test result:` count is non-zero.

**Step 5: Commit**

```bash
git add docs/qpdf-correspondence.md
git commit -m "docs(qpdf): record the ported object warning surface"
```

---

## Out of scope

Declared non-goals, carried verbatim from flpdf-25kg.3.27:

- Object description propagation — qpdf's `Description` / `ChildDescr` and the `$OG` / `$PO` / `$VD` substitutions in `QPDFValue::getDescription`. Tracked as flpdf-25kg.3.28. **Not** the same follow-up as Task 4's deferred `getKey`/`getKeys` warnings: qpdf sets both the context pointer and the description string from the same setters (`libqpdf/qpdf/QPDFValue.hh:60-83`), but the context propagation Task 4 needs is canonical-resolver work, tracked against flpdf-25kg.3.5 (see Task 4). flpdf-25kg.3.28 owns the description *text*, which is a separate, still-open gap even after Task 4's prerequisite lands.
- CLI exit-code aggregation on warnings — flpdf-w1cs.
- Migrating `outline_document_helper.rs`'s existing `&mut Pdf` warning calls.

## Coordination

`flpdf-25kg.3.5` is in progress on `ResolverCore` / `ResolverHandle`. This plan adds one method to `resolver.rs` and one trait impl line; check for conflicts before pushing rather than assuming that file is quiescent.

This branch sits on an unmerged three-PR stack (#669 → #672 → #677). It cannot merge until those do.
