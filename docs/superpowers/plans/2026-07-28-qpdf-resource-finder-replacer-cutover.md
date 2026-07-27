# qpdf ResourceFinder / ResourceReplacer Cutover Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Complete qpdf 11.9.0-compatible `Pl_QPDFTokenizer`, `ResourceFinder`, and `ResourceReplacer` components, migrate every declared production consumer, and delete the handwritten duplicate routes.

**Architecture:** A crate-private `TokenFilter` contract writes through an optional downstream `Pipeline`; `QpdfTokenizer` buffers pipeline chunks and delivers exact qpdf tokens at `finish`. `ResourceFinder` remains a `ParserCallbacks` consumer, while `ResourceReplacer` remains a `TokenFilter`; ContentNormalizer, copied `/DA`, copied appearance streams, and unreferenced-resource pruning compose those components without merging their responsibilities.

**Tech Stack:** Rust workspace, flpdf `Pipeline`, qpdf-shaped tokenizer/parser callbacks, `BTreeMap`/`BTreeSet`, qpdf 11.9.0 C++ oracle probe, Cargo tests, Clippy, rustfmt, cargo-llvm-cov.

## Global Constraints

- qpdf 11.9.0 at commit `3b97c9bd266b7c32ea36d3536e22dab77412886d` is the behavioral oracle.
- Resolve oracle source only with `scripts/fetch-qpdf-source.sh --print-path`; never clone or edit it.
- Keep `TokenFilter`, `QpdfTokenizer`, `ResourceFinder`, and `ResourceReplacer` crate-private.
- Preserve qpdf's separate `ParserCallbacks` and `TokenFilter` responsibilities.
- Preserve every byte outside selected replacement name tokens.
- Preserve resource-pruning scope, Form recursion, `(Form ref, scope owner)` deduplication, cycle handling, inline-image header policy, and conservative retain behavior.
- Do not add compatibility aliases, forwarding wrappers, a generic resource-transformation framework, or deferred object-model token filters.
- Use strict RED -> GREEN -> REFACTOR; no production implementation before its failing test.
- Each commit must pass its focused tests.
- Final changed executable-line patch coverage must be a fresh 100%.

---

## File map

**Create**

- `crates/flpdf/src/token_filter.rs` — shared filter callback and optional pipeline-output helper.
- `crates/flpdf/src/pipeline/qpdf_tokenizer.rs` — `Pl_QPDFTokenizer` lifecycle and byte-to-token delivery.
- `crates/flpdf/src/resource_finder.rs` — qpdf operator/name/offset collection through `ParserCallbacks`.
- `crates/flpdf/src/resource_replacer.rs` — offset-indexed name-token rewrite and shared immediate-transform orchestration.

**Modify**

- `crates/flpdf/src/pipeline.rs` — register `qpdf_tokenizer`.
- `crates/flpdf/src/lib.rs` — register the three new crate-private top-level modules.
- `crates/flpdf/src/content_normalizer.rs` — implement shared `TokenFilter`; delete private runner.
- `crates/flpdf/src/overlay_annotations.rs` — replace `/DA` scanner and local resource map guard.
- `crates/flpdf/src/overlay_appearance_stream.rs` — replace handwritten byte scanner/replacer.
- `crates/flpdf/src/resources.rs` — replace general operator/name classification while retaining pruning-specific traversal.
- `tests/oracle/qpdf_tokenizer_probe.cc` — add `Pl_QPDFTokenizer` lifecycle and `ResourceFinder` record modes.
- `scripts/qpdf-tokenizer-diff.sh` — compile the private qpdf `ResourceFinder.cc` oracle and run new ignored differential tests.
- `scripts/tests/qpdf-tokenizer-diff-contract.sh` — lock the expanded oracle build/run contract.
- `docs/qpdf-correspondence.md` — mark component and consumer correspondence truthfully.

---

### Task 1: Shared TokenFilter and QpdfTokenizer pipeline

**Files:**

- Create: `crates/flpdf/src/token_filter.rs`
- Create: `crates/flpdf/src/pipeline/qpdf_tokenizer.rs`
- Modify: `crates/flpdf/src/pipeline.rs`
- Modify: `crates/flpdf/src/lib.rs`
- Modify: `tests/oracle/qpdf_tokenizer_probe.cc`
- Modify: `scripts/qpdf-tokenizer-diff.sh`
- Modify: `scripts/tests/qpdf-tokenizer-diff-contract.sh`
- Test: `crates/flpdf/src/pipeline/qpdf_tokenizer.rs`

**Interfaces:**

- Consumes: `crate::pipeline::{Pipeline, PipelineError, PipelineResult}` and `crate::tokenizer::{Token, TokenType, Tokenizer}`.
- Produces:

```rust
pub(crate) trait TokenFilter {
    fn handle_token(
        &mut self,
        token: &Token,
        output: &mut TokenFilterOutput<'_>,
    ) -> PipelineResult<()>;

    fn handle_eof(&mut self, _output: &mut TokenFilterOutput<'_>) -> PipelineResult<()> {
        Ok(())
    }
}

pub(crate) struct TokenFilterOutput<'a> {
    next: Option<&'a mut dyn Pipeline>,
}

impl TokenFilterOutput<'_> {
    pub(crate) fn write(&mut self, data: &[u8]) -> PipelineResult<()>;
    pub(crate) fn write_token(&mut self, token: &Token) -> PipelineResult<()>;
}

pub(crate) struct QpdfTokenizer<'a> {
    identifier: String,
    filter: &'a mut dyn TokenFilter,
    next: Option<&'a mut dyn Pipeline>,
    filter_output_attached: bool,
    data: Vec<u8>,
}

impl<'a> QpdfTokenizer<'a> {
    pub(crate) fn new(
        identifier: impl Into<String>,
        filter: &'a mut dyn TokenFilter,
        next: Option<&'a mut dyn Pipeline>,
    ) -> Self;
}
```

- `QpdfTokenizer` implements `Pipeline`.
- Each `finish` takes and clears the buffered input before token delivery, matching
  `Pl_Buffer::getBuffer`. Another `finish` delivers an empty-input EOF cycle and another downstream
  finish, while a later `write` starts a new input cycle.
- Downstream ownership and filter-output attachment are separate. The first successful
  `handle_eof` permanently detaches filter output before downstream `finish`; later cycles still
  deliver callbacks and finish the retained downstream, but filter writes are discarded.
- If a token callback or `handle_eof` fails, the taken input remains consumed, downstream `finish`
  is skipped, and filter output retains its current attachment for the next empty cycle. A
  downstream finish failure occurs after detachment, so retry remains detached.
- Later tasks must not bypass these interfaces with direct `Tokenizer` loops.

- [ ] **Step 1: Extend the oracle probe contract with an exact token-filter record mode**

Add a C++ `RecordingTokenFilter` derived from `QPDFObjectHandle::TokenFilter`. Its
`handleToken` writes the token raw bytes downstream and prints:

```text
token<TAB>TYPE<TAB>RAW_HEX
eof-callback
output<TAB>HEX
```

Add `--mode token-filter`, feed input to `Pl_QPDFTokenizer` in chunks selected by a new
`--chunks` comma-separated argument, and add the argument to the required probe CLI contract.
For all existing modes, pass `--chunks all`.

In `scripts/tests/qpdf-tokenizer-diff-contract.sh`, first add failing assertions that require:

```bash
grep -F 'token-filter' "${PROBE_SOURCE}"
grep -F -- '--chunks' "${PROBE_SOURCE}"
grep -F 'pipeline::qpdf_tokenizer::tests::qpdf_token_filter_differential' "${SCRIPT_SOURCE}"
```

- [ ] **Step 2: Run the oracle script contract and verify RED**

Run:

```bash
bash scripts/tests/qpdf-tokenizer-diff-contract.sh
```

Expected: FAIL because the probe has no `token-filter` mode or `--chunks` option and the shell
driver does not invoke the new ignored Rust differential test.

- [ ] **Step 3: Add failing Rust tests for output/discard, byte coverage, chunking, and timing**

Register the new modules, then add tests in `pipeline/qpdf_tokenizer.rs` using these filter shapes:

```rust
#[derive(Default)]
struct RecordingFilter {
    events: Vec<(TokenType, Vec<u8>)>,
    eof_calls: usize,
}

impl TokenFilter for RecordingFilter {
    fn handle_token(
        &mut self,
        token: &Token,
        output: &mut TokenFilterOutput<'_>,
    ) -> PipelineResult<()> {
        self.events.push((token.token_type, token.raw.clone()));
        output.write_token(token)
    }

    fn handle_eof(&mut self, _output: &mut TokenFilterOutput<'_>) -> PipelineResult<()> {
        self.eof_calls += 1;
        Ok(())
    }
}
```

Tests:

```rust
#[test]
fn chunk_boundaries_do_not_change_tokens_or_output() {
    let input = b"%c\r\nBI /W 1 ID \0/F1 9 Tf EI /F2 12 Tf";
    let one = run_recording(&[input.as_slice()], true).unwrap();
    let bytewise_chunks = input.iter().map(std::slice::from_ref).collect::<Vec<_>>();
    let bytewise = run_recording(&bytewise_chunks, true).unwrap();
    assert_eq!(bytewise, one);
    assert_eq!(one.output, input);
    assert_eq!(one.eof_calls, 1);
    assert_eq!(one.downstream_finishes, 1);
    assert_eq!(
        one.events.iter().map(|(_, raw)| raw.len()).sum::<usize>(),
        input.len()
    );
}

#[test]
fn absent_downstream_discards_filter_output_but_delivers_all_callbacks() {
    let run = run_recording(&[b"/F1 12 Tf"], false).unwrap();
    assert!(run.output.is_empty());
    assert_eq!(run.eof_calls, 1);
    assert_eq!(run.events.last().unwrap().0, TokenType::Eof);
}

#[test]
fn filter_failure_does_not_finish_downstream() {
    let mut sink = RecordingSink::default();
    let mut filter = FailOnWord("Tf");
    let mut stage = QpdfTokenizer::new("token filter", &mut filter, Some(&mut sink));
    stage.write(b"/F1 12 Tf").unwrap();
    assert_eq!(stage.finish().unwrap_err().message(), "filter failed at Tf");
    drop(stage);
    assert_eq!(sink.finishes, 0);
}

#[test]
fn downstream_finish_failure_is_returned_after_eof_callback() {
    let mut sink = FinishFailSink::default();
    let mut filter = RecordingFilter::default();
    let mut stage = QpdfTokenizer::new("token filter", &mut filter, Some(&mut sink));
    stage.write(b"q").unwrap();
    assert_eq!(stage.finish().unwrap_err().message(), "sink finish failed");
    assert_eq!(filter.eof_calls, 1);
}

#[test]
fn dropping_without_finish_does_not_finish_downstream() {
    let mut sink = RecordingSink::default();
    let mut filter = RecordingFilter::default();
    {
        let mut stage = QpdfTokenizer::new("token filter", &mut filter, Some(&mut sink));
        stage.write(b"q").unwrap();
    }
    assert_eq!(sink.finishes, 0);
    assert!(filter.events.is_empty());
}

#[test]
fn handle_eof_failure_does_not_finish_downstream() {
    let mut sink = RecordingSink::default();
    let mut filter = FailOnEof;
    let mut stage = QpdfTokenizer::new("token filter", &mut filter, Some(&mut sink));
    stage.write(b"q").unwrap();
    assert_eq!(stage.finish().unwrap_err().message(), "filter EOF failed");
    drop(stage);
    assert_eq!(sink.finishes, 0);
}
```

Add an ignored `qpdf_token_filter_differential` test that compares qpdf probe records for:

- empty input;
- comments and all PDF whitespace bytes;
- escaped names and strings;
- terminal bad token;
- terminal `ID`;
- inline-image false-`EI` candidates;
- one chunk, bytewise chunks, and split points around `ID` and `EI`.

Add a separate ignored `qpdf_token_filter_lifecycle_differential` mode that performs repeated
`finish`, write-after-finish, and fail-once downstream-finish retry. It records callback counts,
finish attempts, and output bytes, proving that qpdf forwards output only through the first
successful `handle_eof` cycle.

- [ ] **Step 4: Run the focused Rust tests and verify RED**

Run:

```bash
cargo test -p flpdf --lib pipeline::qpdf_tokenizer::tests -- --nocapture
```

Expected: compile FAIL because `TokenFilter`, `TokenFilterOutput`, and `QpdfTokenizer` have only
module declarations and no implementation.

- [ ] **Step 5: Implement TokenFilterOutput**

Implement:

```rust
impl<'a> TokenFilterOutput<'a> {
    pub(crate) fn new(next: Option<&'a mut dyn Pipeline>) -> Self {
        Self { next }
    }

    pub(crate) fn write(&mut self, data: &[u8]) -> PipelineResult<()> {
        if data.is_empty() {
            return Ok(());
        }
        match self.next.as_deref_mut() {
            Some(next) => next.write(data),
            None => Ok(()),
        }
    }

    pub(crate) fn write_token(&mut self, token: &Token) -> PipelineResult<()> {
        self.write(&token.raw)
    }
}
```

Keep `finish` owned by `QpdfTokenizer`, not by the output helper or filter.

- [ ] **Step 6: Implement buffered QpdfTokenizer finish delivery**

Implement `Pipeline` so `write` appends bytes. At `finish`, first take the internal buffer, create
`Tokenizer::new(&input)`, enable EOF and ignorable tokens, and run this sequence:

```rust
let input = std::mem::take(&mut self.data);
let mut tokenizer = Tokenizer::new(&input);
loop {
    let token = tokenizer
        .read_token(true, 0)
        .map_err(|error| PipelineError::runtime(format!("{}: {error}", self.identifier)))?;
    let is_eof = token.token_type == TokenType::Eof;
    let is_id = token.is_word_value(b"ID");
    self.handle_token(&token)?;
    if is_eof {
        break;
    }
    if is_id {
        let separator = tokenizer.consume_one_byte_or(b' ');
        let space = Token::new(TokenType::Space, vec![separator]);
        self.handle_token(&space)?;
        tokenizer
            .expect_inline_image()
            .map_err(|error| PipelineError::logic(format!("{}: {error:?}", self.identifier)))?;
    }
}
self.handle_eof()?; // permanently detaches filter output on success
if let Some(next) = self.next.as_deref_mut() {
    next.finish()?;
}
Ok(())
```

Implement `handle_token`/`handle_eof` helpers that lend `next` to `TokenFilterOutput` only while
`filter_output_attached` is true and always restore downstream ownership before returning a
callback result. Add tests proving repeated finish and write-after-finish still deliver callbacks
and downstream finishes but emit output only in the first cycle; callback failures retain
attachment, while downstream-finish failure retry remains detached.

- [ ] **Step 7: Complete the C++ probe, shell driver, and contract**

Implement `token-filter`, `token-filter-lifecycle`, and `--chunks`. Update every existing probe
invocation in Rust tests to pass `--chunks all`. Add both ignored differential invocations to
`scripts/qpdf-tokenizer-diff.sh`.

Run:

```bash
bash scripts/tests/qpdf-tokenizer-diff-contract.sh
scripts/qpdf-tokenizer-diff.sh
```

Expected: PASS; the live qpdf records match all Rust cases.

- [ ] **Step 8: Run focused tests and commit**

Run:

```bash
cargo test -p flpdf --lib pipeline::qpdf_tokenizer::tests
```

Expected: PASS.

Commit:

```bash
git add crates/flpdf/src/token_filter.rs crates/flpdf/src/pipeline/qpdf_tokenizer.rs \
  crates/flpdf/src/pipeline.rs crates/flpdf/src/lib.rs \
  tests/oracle/qpdf_tokenizer_probe.cc scripts/qpdf-tokenizer-diff.sh \
  scripts/tests/qpdf-tokenizer-diff-contract.sh
git commit -m "feat(pipeline): add qpdf tokenizer stage"
```

---

### Task 2: ResourceFinder parser component

**Files:**

- Create: `crates/flpdf/src/resource_finder.rs`
- Modify: `crates/flpdf/src/lib.rs`
- Modify: `tests/oracle/qpdf_tokenizer_probe.cc`
- Modify: `scripts/qpdf-tokenizer-diff.sh`
- Modify: `scripts/tests/qpdf-tokenizer-diff-contract.sh`
- Test: `crates/flpdf/src/resource_finder.rs`

**Interfaces:**

- Consumes: `crate::content_stream::{ParseControl, ParserCallbacks}` and `crate::{Object, Result}`.
- Produces:

```rust
pub(crate) type ResourceNamesByType =
    BTreeMap<Vec<u8>, BTreeMap<Vec<u8>, BTreeSet<usize>>>;

#[derive(Debug, Default)]
pub(crate) struct ResourceFinder {
    last_name: Option<(Vec<u8>, usize)>,
    names_by_resource_type: ResourceNamesByType,
    had_diagnostics: bool,
}

impl ResourceFinder {
    pub(crate) fn names_by_resource_type(&self) -> &ResourceNamesByType;
    pub(crate) fn had_diagnostics(&self) -> bool;
}
```

- [ ] **Step 1: Add failing operator/name/offset tests**

Add this test helper:

```rust
fn find(input: &[u8]) -> Result<ResourceFinder> {
    let mut finder = ResourceFinder::default();
    parse_content_stream_data(input, &mut finder)?;
    Ok(finder)
}
```

Then add:

```rust
#[test]
fn records_qpdf_operator_table_with_raw_name_offsets() {
    let input = b"/CS1 CS /cs1 cs /GS1 gs /F1 12 Tf /P1 SCN /p1 scn \
                  /Span /MC1 BDC /Span /MC2 DP /Sh1 sh /X1 Do";
    let finder = find(input).unwrap();
    assert_eq!(finder.names_by_resource_type()[b"Font".as_slice()][b"F1".as_slice()],
               BTreeSet::from([input.windows(3).position(|w| w == b"/F1").unwrap()]));
    assert_eq!(finder.names_by_resource_type()[b"XObject".as_slice()]
               .keys().cloned().collect::<Vec<_>>(), vec![b"X1".to_vec()]);
    let flat_names = finder.names_by_resource_type().values()
        .flat_map(|by_name| by_name.keys()).collect::<BTreeSet<_>>();
    assert_eq!(flat_names.len(), 10);
}

#[test]
fn last_name_survives_non_name_operands_and_resource_operators() {
    let finder = find(b"/F1 12 Tf 99 Tf").unwrap();
    assert_eq!(
        finder.names_by_resource_type()[b"Font".as_slice()][b"F1".as_slice()].len(),
        2
    );
}

#[test]
fn final_name_before_bdc_and_dp_is_the_properties_name() {
    let finder = find(b"/Span /MC1 BDC /Tag /MC2 DP").unwrap();
    assert!(finder.names_by_resource_type()[b"Properties".as_slice()]
        .contains_key(b"MC1".as_slice()));
    assert!(finder.names_by_resource_type()[b"Properties".as_slice()]
        .contains_key(b"MC2".as_slice()));
}

#[test]
fn parser_diagnostics_mark_results_incomplete() {
    let finder = find(b"<0g> /F1 12 Tf").unwrap();
    assert!(finder.had_diagnostics());
}
```

- [ ] **Step 2: Run and verify RED**

Run:

```bash
cargo test -p flpdf --lib resource_finder::tests
```

Expected: compile FAIL because `ResourceFinder` and its result accessors do not exist.

- [ ] **Step 3: Implement ResourceFinder**

Use one table function:

```rust
fn resource_type_for_operator(operator: &[u8]) -> Option<&'static [u8]> {
    match operator {
        b"CS" | b"cs" => Some(b"ColorSpace"),
        b"gs" => Some(b"ExtGState"),
        b"Tf" => Some(b"Font"),
        b"SCN" | b"scn" => Some(b"Pattern"),
        b"BDC" | b"DP" => Some(b"Properties"),
        b"sh" => Some(b"Shading"),
        b"Do" => Some(b"XObject"),
        _ => None,
    }
}
```

In `handle_object`, record `Object::Name(name)` as `(name, offset)`. For
`Object::Operator(operator)`, if both the table lookup and `last_name` exist, insert the name and
offset into `names_by_resource_type`. Do not clear `last_name`. Ignore other objects. Set
`had_diagnostics = true` from `handle_diagnostic`; `handle_eof` returns `Ok(())`. The qpdf
`getNames()`-shaped flat oracle view is derived in tests from the categorized map's key union; do
not retain a duplicate production flat set.

- [ ] **Step 4: Add and run a live ResourceFinder oracle mode**

Add `--mode resource-finder` to the C++ probe. Use qpdf's private `ResourceFinder` directly and
print sorted records:

```text
name<TAB>NAME_HEX
resource<TAB>TYPE_HEX<TAB>NAME_HEX<TAB>OFFSET
```

Compile `${qpdf_source}/libqpdf/ResourceFinder.cc` into the probe command. Update the shell
contract test to require that source argument and the new ignored Rust differential:

```text
resource_finder::tests::qpdf_resource_finder_differential
```

Run:

```bash
bash scripts/tests/qpdf-tokenizer-diff-contract.sh
scripts/qpdf-tokenizer-diff.sh
```

Expected: PASS for all ten operators, repeated uses, escaped names, malformed content, comments,
and inline images.

- [ ] **Step 5: Run focused tests and commit**

Run:

```bash
cargo test -p flpdf --lib resource_finder::tests
```

Expected: PASS.

Commit:

```bash
git add crates/flpdf/src/resource_finder.rs crates/flpdf/src/lib.rs \
  tests/oracle/qpdf_tokenizer_probe.cc scripts/qpdf-tokenizer-diff.sh \
  scripts/tests/qpdf-tokenizer-diff-contract.sh
git commit -m "feat(content): add qpdf resource finder"
```

---

### Task 3: ResourceReplacer token filter

**Files:**

- Create: `crates/flpdf/src/resource_replacer.rs`
- Modify: `crates/flpdf/src/lib.rs`
- Test: `crates/flpdf/src/resource_replacer.rs`

**Interfaces:**

- Consumes:

```rust
pub(crate) type ResourceNamesByType =
    BTreeMap<Vec<u8>, BTreeMap<Vec<u8>, BTreeSet<usize>>>;
```

- Produces:

```rust
pub(crate) type ResourceRenames =
    BTreeMap<Vec<u8>, BTreeMap<Vec<u8>, Vec<u8>>>;

pub(crate) struct ResourceReplacer {
    offset: usize,
    to_replace: BTreeMap<Vec<u8>, BTreeMap<usize, Vec<u8>>>,
}

impl ResourceReplacer {
    pub(crate) fn new(
        renames: &ResourceRenames,
        names: &ResourceNamesByType,
    ) -> Self;
}

impl TokenFilter for ResourceReplacer { /* qpdf offset behavior */ }

pub(crate) fn replace_resource_names(
    input: &[u8],
    renames: &ResourceRenames,
) -> crate::Result<Option<Vec<u8>>>;
```

`replace_resource_names` returns `Ok(None)` when `parse_content_stream_data` fails or finder
diagnostics make offsets incomplete. It returns `Ok(Some(bytes))` for a complete scan, including
the identity case.

- [ ] **Step 1: Add failing offset-selected replacement tests**

Add:

```rust
#[test]
fn rewrites_only_name_and_offset_pairs_selected_by_finder() {
    let input = b"/F1 9 Tf /F1 10 Tj /F1 11 Tf";
    let mut renames = ResourceRenames::new();
    renames.entry(b"Font".to_vec()).or_default()
        .insert(b"F1".to_vec(), b"F A_1".to_vec());
    assert_eq!(
        replace_resource_names(input, &renames).unwrap().unwrap(),
        b"/F#20A_1 9 Tf /F1 10 Tj /F#20A_1 11 Tf"
    );
}

#[test]
fn replacement_length_does_not_shift_source_offset_matching() {
    let input = b"/A 1 Tf /A 2 Tf";
    let mut renames = ResourceRenames::new();
    renames.entry(b"Font".to_vec()).or_default()
        .insert(b"A".to_vec(), b"MuchLonger".to_vec());
    assert_eq!(
        replace_resource_names(input, &renames).unwrap().unwrap(),
        b"/MuchLonger 1 Tf /MuchLonger 2 Tf"
    );
}

#[test]
fn inline_image_payload_and_unselected_tokens_are_byte_identical() {
    let input = b"%c\r\nBI ID /F1 8 Tf EI /F1 9 Tf";
    let renames = font_renames(b"F1", b"F2");
    assert_eq!(
        replace_resource_names(input, &renames).unwrap().unwrap(),
        b"%c\r\nBI ID /F1 8 Tf EI /F2 9 Tf"
    );
}

#[test]
fn incomplete_finder_returns_none_without_partial_replacement() {
    let renames = font_renames(b"F1", b"F2");
    assert!(replace_resource_names(b"<0g> /F1 9 Tf", &renames)
        .unwrap().is_none());
}
```

- [ ] **Step 2: Run and verify RED**

Run:

```bash
cargo test -p flpdf --lib resource_replacer::tests
```

Expected: compile FAIL because `ResourceReplacer`, `ResourceRenames`, and
`replace_resource_names` do not exist.

- [ ] **Step 3: Implement the replacement index and TokenFilter**

Build `to_replace` by crossing the two outer resource-type maps, then old-name maps, then offsets.
Implement:

```rust
fn handle_token(
    &mut self,
    token: &Token,
    output: &mut TokenFilterOutput<'_>,
) -> PipelineResult<()> {
    let replacement = (token.token_type == TokenType::Name)
        .then(|| self.to_replace.get(&token.value))
        .flatten()
        .and_then(|offsets| offsets.get(&self.offset));
    if let Some(new_name) = replacement {
        let replacement = name_token_from_decoded_body(new_name);
        output.write_token(&replacement)?;
        self.advance_offset(token)?;
    } else {
        self.advance_offset(token)?;
        output.write_token(token)?;
    }
    Ok(())
}
```

Build both lookup keys and replacement tokens from decoded name bodies by prepending the canonical
name sigil without stripping a leading body slash; the serializer then escapes that body slash as
`#2f`. Match qpdf's failure ordering exactly: a replacement advances after its write succeeds,
while a non-replacement advances before attempting the raw write. `handle_eof` keeps the default
no-op. The EOF raw length is zero.

- [ ] **Step 4: Implement shared immediate orchestration**

`replace_resource_names` must:

1. return `Ok(Some(input.to_vec()))` immediately for empty renames;
2. parse through `ResourceFinder`;
3. return `Ok(None)` on parser error or `had_diagnostics`;
4. create `Buffer`, `ResourceReplacer`, and `QpdfTokenizer`;
5. write the complete input, finish the tokenizer, drop it, and take the finished buffer;
6. convert `PipelineError` through the existing `From<PipelineError> for Error`.

- [ ] **Step 5: Run focused tests and commit**

Run:

```bash
cargo test -p flpdf --lib resource_replacer::tests
```

Expected: PASS.

Commit:

```bash
git add crates/flpdf/src/resource_replacer.rs crates/flpdf/src/lib.rs
git commit -m "feat(content): add qpdf resource replacer"
```

---

### Task 4: ContentNormalizer production cutover

**Files:**

- Modify: `crates/flpdf/src/content_normalizer.rs`
- Test: `crates/flpdf/src/content_normalizer.rs`

**Interfaces:**

- Consumes: `TokenFilter`, `TokenFilterOutput`, `QpdfTokenizer`, and `pipeline::buffer::Buffer`.
- Produces: unchanged public `normalize_content_stream(&[u8]) -> ContentNormalization`.

- [ ] **Step 1: Change the runner-order test to require the shared pipeline**

Replace the private-runner-only test with:

```rust
#[test]
fn shared_pipeline_delivers_eof_token_before_handle_eof() {
    let mut output = Buffer::new("normalized", None);
    let mut filter = RecordingFilter::default();
    {
        let mut tokenizer = QpdfTokenizer::new("normalizer", &mut filter, Some(&mut output));
        tokenizer.write(b"q").unwrap();
        tokenizer.finish().unwrap();
    }
    assert_eq!(
        filter.0,
        vec![TokenType::Word, TokenType::Eof, TokenType::BraceOpen]
    );
}
```

Change `RecordingFilter` to the shared trait signature and write raw tokens through output.

- [ ] **Step 2: Run and verify RED**

Run:

```bash
cargo test -p flpdf --lib content_normalizer::tests::shared_pipeline_delivers_eof_token_before_handle_eof -- --exact
```

Expected: compile FAIL because `ContentNormalizer` still implements its private `TokenFilter` and
the test requires the shared callback signature.

- [ ] **Step 3: Migrate ContentNormalizer**

Delete the private `TokenFilter` trait and `run_token_filter`. Change the implementation:

```rust
impl TokenFilter for ContentNormalizer {
    fn handle_token(
        &mut self,
        token: &Token,
        output: &mut TokenFilterOutput<'_>,
    ) -> PipelineResult<()> {
        if token.token_type == TokenType::Bad {
            self.any_bad_tokens = true;
            self.last_token_was_bad = true;
        } else if token.token_type != TokenType::Eof {
            self.last_token_was_bad = false;
        }

        match token.token_type {
            TokenType::Space => self.write_space(&token.raw, output)?,
            TokenType::String | TokenType::Name => {
                output.write_token(&Token::new(token.token_type, token.value.clone()))?;
            }
            _ => output.write_token(token)?,
        }

        if matches!(token.token_type, TokenType::String | TokenType::Name)
            && token.raw.iter().any(|byte| matches!(*byte, b'\r' | b'\n'))
        {
            output.write(b"\n")?;
        }
        Ok(())
    }
}
```

Change `write_space` to accept `&mut TokenFilterOutput<'_>` and return `PipelineResult<()>`,
using this byte-exact loop:

```rust
fn write_space(
    &mut self,
    raw: &[u8],
    output: &mut TokenFilterOutput<'_>,
) -> PipelineResult<()> {
    for (index, &byte) in raw.iter().enumerate() {
        if byte == b'\r' {
            if raw.get(index + 1) != Some(&b'\n') {
                output.write(b"\n")?;
            }
        } else {
            output.write(std::slice::from_ref(&byte))?;
        }
    }
    Ok(())
}
```

Remove `output: Vec<u8>` from `ContentNormalizer`. Change its terminal conversion to:

```rust
fn finish(self, bytes: Vec<u8>) -> ContentNormalization {
    ContentNormalization {
        bytes,
        any_bad_tokens: self.any_bad_tokens,
        last_token_was_bad: self.last_token_was_bad,
    }
}
```

Build `normalize_content_stream` through `QpdfTokenizer -> Buffer`. The only possible failures are
internal component contract failures, so use explicit `expect` messages:

```rust
let mut output = Buffer::new("content normalizer output", None);
let mut normalizer = ContentNormalizer::default();
{
    let mut tokenizer = QpdfTokenizer::new(
        "content normalizer",
        &mut normalizer,
        Some(&mut output),
    );
    tokenizer.write(input).expect("buffer-backed tokenizer write is infallible");
    tokenizer.finish().expect("allow-bad tokenizer finish is infallible");
}
let bytes = output.take_buffer().expect("finished output buffer is ready");
normalizer.finish(bytes)
```

- [ ] **Step 4: Run focused and live oracle tests**

Run:

```bash
cargo test -p flpdf --lib content_normalizer::tests
scripts/qpdf-tokenizer-diff.sh
```

Expected: PASS with unchanged oracle records.

- [ ] **Step 5: Prove the old route is gone and commit**

Run:

```bash
rg -n "trait TokenFilter|fn run_token_filter" crates/flpdf/src/content_normalizer.rs
```

Expected: no matches.

Commit:

```bash
git add crates/flpdf/src/content_normalizer.rs
git commit -m "refactor(content): cut normalizer over to tokenizer pipeline"
```

---

### Task 5: Copied-field /DA cutover

**Files:**

- Modify: `crates/flpdf/src/overlay_annotations.rs`
- Test: `crates/flpdf/src/overlay_annotations.rs`

**Interfaces:**

- Consumes: `resource_replacer::{replace_resource_names, ResourceRenames}`.
- Produces: `DrMap::renames(&self) -> &ResourceRenames`.

- [ ] **Step 1: Add a RED regression for qpdf's no-local-resource-guard behavior**

Replace the current test that expects an absent `/DR` key to remain unchanged:

```rust
#[test]
fn adjust_default_appearance_rewrites_without_local_resource_presence_guard() {
    let dr_map = category_dr_map(b"Font", &[("F1", "F1_1")]);
    let resources = BTreeMap::new();
    assert_eq!(
        adjust_default_appearance(b"/F1 12 Tf", &dr_map, &resources),
        b"/F1_1 12 Tf"
    );
}
```

Add:

```rust
#[test]
fn malformed_da_is_retained_without_partial_replacement() {
    let dr_map = category_dr_map(b"Font", &[("F1", "F1_1")]);
    let resources = category_resources_map(b"Font", &["F1"]);
    assert_eq!(
        adjust_default_appearance(b"<0g> /F1 12 Tf", &dr_map, &resources),
        b"<0g> /F1 12 Tf"
    );
}
```

- [ ] **Step 2: Run and verify RED**

Run:

```bash
cargo test -p flpdf --lib \
  overlay_annotations::tests::adjust_default_appearance_rewrites_without_local_resource_presence_guard \
  -- --exact
```

Expected: FAIL because the existing local-resources presence guard keeps `/F1` unchanged.

- [ ] **Step 3: Expose DrMap renames and replace the scanner**

Add:

```rust
pub(crate) fn renames(&self) -> &ResourceRenames {
    &self.by_name
}
```

Change:

```rust
fn adjust_default_appearance(da: &[u8], dr_map: &DrMap) -> Result<Vec<u8>> {
    Ok(replace_resource_names(da, dr_map.renames())?
        .unwrap_or_else(|| da.to_vec()))
}
```

Delete:

- the `per_category_resources` parameter and its caller-only construction;
- the local byte scanner;
- `da_operator_category`;
- imports used only by the scanner.

Update direct and indirect `/DA` caller arms to propagate `?`.

- [ ] **Step 4: Run focused and overlay tests**

Run:

```bash
cargo test -p flpdf --lib overlay_annotations::tests
cargo test -p flpdf --lib overlay::byte_gate
cargo test -p flpdf-cli --test cli_overlay
```

Expected: PASS. If an existing unit test encoded the removed guard, update only its expected qpdf
result; do not restore the guard.

- [ ] **Step 5: Prove duplicate scanner symbols are gone and commit**

Run:

```bash
rg -n "da_operator_category|starts_number_token|per_category_resources" \
  crates/flpdf/src/overlay_annotations.rs
```

Expected: no production matches.

Commit:

```bash
git add crates/flpdf/src/overlay_annotations.rs
git commit -m "refactor(overlay): cut default appearance over to resource replacer"
```

---

### Task 6: Copied appearance-stream cutover

**Files:**

- Modify: `crates/flpdf/src/overlay_appearance_stream.rs`
- Test: `crates/flpdf/src/overlay_appearance_stream.rs`

**Interfaces:**

- Consumes: `replace_resource_names(decoded, local_dr_map.renames())`.
- Produces:

```rust
fn rewrite_appearance_content(decoded: &[u8], dr_map: &DrMap) -> Vec<u8>;
```

The helper preserves `adjust_appearance_stream` and is its only content-rewrite route.

- [ ] **Step 1: Add a RED test that requires shared incomplete-scan behavior**

Change the malformed-token test to assert no partial rewrite:

```rust
#[test]
fn resource_replacement_retains_all_bytes_when_scan_is_incomplete() {
    let dr_map = DrMap::for_test(b"Font", b"F1", b"F1_1");
    let content = b"<0g> /F1 12 Tf";
    assert_eq!(rewrite_appearance_content(content, &dr_map), content);
}
```

- [ ] **Step 2: Run and verify RED**

Run:

```bash
cargo test -p flpdf --lib \
  overlay_appearance_stream::tests::resource_replacement_retains_all_bytes_when_scan_is_incomplete \
  -- --exact
```

Expected: compile FAIL because `rewrite_appearance_content` does not exist and production still
calls the handwritten tolerant scanner.

- [ ] **Step 3: Replace the handwritten scanner**

Implement:

```rust
fn rewrite_appearance_content(decoded: &[u8], dr_map: &DrMap) -> Vec<u8> {
    match replace_resource_names(decoded, dr_map.renames()) {
        Ok(Some(bytes)) => bytes,
        Ok(None) | Err(_) => decoded.to_vec(),
    }
}
```

Inside `adjust_appearance_stream`, replace:

```rust
let new_decoded = resource_replacer(&decoded, &local_dr_map);
```

with:

```rust
let new_decoded = rewrite_appearance_content(&decoded, &local_dr_map);
```

Preserve the existing best-effort decode/re-encode behavior, `/Length` update,
resource-dictionary privatization, and second-order rename handling.

Delete:

- `resource_replacer`;
- `resource_type_for_operator`;
- `find_inline_image_ei`;
- `next_delimiter_bounded_ei`;
- `ei_lookahead_passes`;
- scanner-only imports and tests.

Move behavior assertions that remain component requirements to `resource_replacer.rs` before
deleting their old copies.

- [ ] **Step 4: Run focused and byte-gate tests**

Run:

```bash
cargo test -p flpdf --lib resource_replacer::tests
cargo test -p flpdf --lib overlay_appearance_stream::tests
cargo test -p flpdf --lib overlay::byte_gate
cargo test -p flpdf-cli --test cli_overlay
```

Expected: PASS.

- [ ] **Step 5: Prove the old route is gone and commit**

Run:

```bash
rg -n "resource_type_for_operator|find_inline_image_ei|next_delimiter_bounded_ei|ei_lookahead_passes|fn resource_replacer" \
  crates/flpdf/src/overlay_appearance_stream.rs
```

Expected: no matches.

Commit:

```bash
git add crates/flpdf/src/overlay_appearance_stream.rs \
  crates/flpdf/src/resource_replacer.rs
git commit -m "refactor(overlay): cut appearance streams over to resource replacer"
```

---

### Task 7: Unreferenced-resource pruning cutover

**Files:**

- Modify: `crates/flpdf/src/resources.rs`
- Modify: `crates/flpdf/src/resource_finder.rs`
- Test: `crates/flpdf/src/resources.rs`

**Interfaces:**

- Consumes: `ResourceFinder::names_by_resource_type()` and `had_diagnostics()`.
- Produces: unchanged public `remove_unreferenced_resources`.

- [ ] **Step 1: Add RED tests for shared qpdf last-name classification**

Use the existing `build_page_with_resources_carrier_pdf` and `collect_test_content` helpers:

```rust
#[test]
fn pruning_uses_shared_resource_finder_last_name_semantics() {
    let bytes = build_page_with_resources_carrier_pdf(
        "<< /Type /Page /MediaBox [0 0 612 792] >>",
        "null",
    );
    let mut pdf = Pdf::open(Cursor::new(bytes)).expect("PDF should parse");
    let (complete, used) =
        collect_test_content(&mut pdf, b"/Shared 12 Tf 99 gs", None).unwrap();
    assert!(complete);
    assert!(used[b"Font".as_slice()].contains(b"Shared".as_slice()));
    assert!(used[b"ExtGState".as_slice()].contains(b"Shared".as_slice()));
}

#[test]
fn pruning_recurses_each_shared_finder_xobject_name() {
    let bytes = build_page_with_resources_carrier_pdf(
        "<< /Type /Page /MediaBox [0 0 612 792] >>",
        "<0g>",
    );
    let mut pdf = Pdf::open(Cursor::new(bytes)).expect("PDF should parse");
    let mut xobjects = Dictionary::new();
    xobjects.insert("Fm0", Object::Reference(ObjectRef::new(4, 0)));
    let mut resources = Dictionary::new();
    resources.insert("XObject", Object::Dictionary(xobjects));

    let error = collect_test_content(
        &mut pdf,
        b"/Fm0 12 Tf 99 Do",
        Some(&resources),
    )
    .expect_err("retained last name must route Do to the malformed Form");
    assert!(matches!(error, Error::Parse { .. }));
}
```

- [ ] **Step 2: Run and verify RED**

Run:

```bash
cargo test -p flpdf --lib \
  resources::tests::pruning_uses_shared_resource_finder_last_name_semantics \
  -- --exact
```

Expected: FAIL because the current operand-vector classifier does not reuse qpdf's retained
`last_name` for the second `Tf`.

- [ ] **Step 3: Replace ResourceCallbacks' general classification**

Change `ResourceCallbacks` to contain:

```rust
finder: ResourceFinder,
inline_header: Option<Vec<Object>>,
valid_xobjects: Vec<Vec<u8>>,
complete: bool,
```

For every object callback:

1. clone and pass the object, offset, and length to `finder.handle_object`;
2. retain only the pruning-specific `BI`/`ID` header validation and inline ColorSpace collection;
3. do not classify ordinary resource operators locally.

For each ordinary `Do` event received while collection is still complete and outside an inline
header, append the finder's current name to `valid_xobjects`. A diagnostic permanently closes this
valid traversal prefix. After parsing:

```rust
let mut complete =
    parse_result.is_ok() && !callbacks.finder.had_diagnostics() && callbacks.complete;
let names = callbacks.finder.names_by_resource_type();
if complete {
    record_direct_names(ctx.used, names, scope.record_direct);
}
let mut traversed = BTreeSet::new();
for name in &callbacks.valid_xobjects {
    if !traversed.insert(name.as_slice()) {
        continue;
    }
    if !recurse_form_xobject(ctx, name, scope, depth)? {
        complete = false;
        break;
    }
}
Ok(complete)
```

`record_direct_names` copies all seven categories when `record_direct` is true, skips builtin
`ColorSpace` names with the existing predicate, and does not record direct names for own-resources
Forms.

Delete `process_operator` and the ordinary `operands` field. Keep inline-image header pair
validation because `ResourceFinder` intentionally has no `ID -> ColorSpace` rule.

- [ ] **Step 4: Preserve structural error ordering explicitly**

Traverse the callback-order `valid_xobjects` prefix, deduplicated by name. Do not rebuild traversal
from every name in the final finder map after incomplete parsing: `/Fm0 Do <0g>` must still surface
the earlier Form/object structural error, while `<0g> /Fm0 Do` must stop before the later `Do`.
Likewise, operators seen inside an invalid inline-image header and `Do`-looking bytes in an opaque
inline-image payload must never become Form traversals.

- [ ] **Step 5: Run focused resource-pruning tests**

Run:

```bash
cargo test -p flpdf --lib resources::tests
cargo test -p flpdf --test resource_pruning_tests
cargo test -p flpdf-cli --test cli_optimization_matrix
```

Expected: PASS for page/Form scopes, own-resources Forms, resource-less Forms, cycles, DAGs,
malformed content, inline images, and CLI pruning modes.

- [ ] **Step 6: Prove the duplicate table is gone and commit**

Run:

```bash
rg -n 'b"Tf" =>|b"gs" =>|b"cs" \\| b"CS"|b"scn" \\| b"SCN"|b"BDC" \\| b"DP"|fn process_operator' \
  crates/flpdf/src/resources.rs
```

Expected: no general resource-operator table matches; inline-image `/CS` key handling may remain.

Commit:

```bash
git add crates/flpdf/src/resources.rs crates/flpdf/src/resource_finder.rs
git commit -m "refactor(resources): use shared resource finder"
```

---

### Task 8: Correspondence, cleanup, and complete verification

**Files:**

- Modify: `docs/qpdf-correspondence.md`
- Verify: every changed file from Tasks 1-7

**Interfaces:**

- Consumes: completed production cutover and fresh verification evidence.
- Produces: truthful qpdf correspondence and a branch ready for review.

- [ ] **Step 1: Update correspondence and run old-symbol inventory**

Document:

- `Pl_QPDFTokenizer` -> `pipeline/qpdf_tokenizer.rs`;
- `ResourceFinder` -> `resource_finder.rs`;
- anonymous `QPDFAcroFormDocumentHelper::ResourceReplacer` -> `resource_replacer.rs`;
- ContentNormalizer, `/DA`, AP streams, and resource pruning as production consumers.

Run:

```bash
rg -n "run_token_filter|da_operator_category|resource_type_for_operator|find_inline_image_ei|next_delimiter_bounded_ei|ei_lookahead_passes|fn resource_replacer|fn process_operator" \
  crates/flpdf/src
```

Expected: no old production implementation symbols.

- [ ] **Step 2: Run formatting**

Run:

```bash
cargo fmt --all
cargo fmt --all -- --check
```

Expected: PASS with no diff from the check.

- [ ] **Step 3: Run all focused oracle and consumer gates**

Run:

```bash
bash scripts/tests/qpdf-tokenizer-diff-contract.sh
scripts/qpdf-tokenizer-diff.sh
cargo test -p flpdf --lib pipeline::qpdf_tokenizer::tests
cargo test -p flpdf --lib resource_finder::tests
cargo test -p flpdf --lib resource_replacer::tests
cargo test -p flpdf --lib content_normalizer::tests
cargo test -p flpdf --lib overlay_annotations::tests
cargo test -p flpdf --lib overlay_appearance_stream::tests
cargo test -p flpdf --lib resources::tests
cargo test -p flpdf --lib overlay::byte_gate
cargo test -p flpdf --test resource_pruning_tests
cargo test -p flpdf-cli --test cli_overlay
cargo test -p flpdf-cli --test cli_optimization_matrix
```

Expected: PASS.

- [ ] **Step 4: Run workspace lint and tests**

Run:

```bash
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test
```

Expected: PASS with no warnings or failures.

- [ ] **Step 5: Generate fresh coverage**

Run:

```bash
cargo llvm-cov clean --workspace
cargo llvm-cov --workspace --features qpdf-zlib-compat --ignore-run-fail \
  --lcov --output-path /tmp/flpdf-qynx-3.lcov
scripts/patch-coverage.sh --base origin/main --lcov /tmp/flpdf-qynx-3.lcov
```

Expected: changed executable-line coverage is exactly 100%. An uncovered executable line fails
this task and requires another RED/GREEN test cycle followed by a clean coverage regeneration.

- [ ] **Step 6: Commit correspondence documentation**

Run:

```bash
git status --short
git diff --check
```

Commit only the files actually changed:

```bash
git add docs/qpdf-correspondence.md
git commit -m "docs: record resource pipeline cutover"
```

- [ ] **Step 7: Final readback before publication**

Run:

```bash
git status --short --branch
git log --oneline origin/main..HEAD
bd show flpdf-qynx.3
```

Expected:

- clean feature worktree;
- only scoped commits after `origin/main`;
- Bead remains `in_progress` until review/publication is complete;
- acceptance criteria are all evidenced locally.

Do not close the Bead or push git/Beads until the user-selected execution workflow reaches its
publication checkpoint.
