# Canonical resolver ownership design (flpdf-25kg.3.5)

> **For Claude:** this is a durable-decision document under `AGENTS.md` §7. The
> qpdf oracle facts below were verified against pinned qpdf 11.9.0 source; review
> them for correctness like any other claim.

**Decision:** fold `flpdf-k8ln` into `flpdf-25kg.3.5`. Build the canonical
resolver in one cutover, introduce no resolver bridge, and strip the legacy
routes rather than preserving them.

## Why the attach cannot be a thin slice

`flpdf-k8ln` ("wire Pdf resolver into nested ObjectHandle dereference") reads
like plumbing that could land ahead of `.3.5`. It cannot.

`DocumentResolver::resolve_indirect` takes `&self` (`object_handle.rs:36-38`),
and a handle holds `Option<Weak<dyn DocumentResolver>>` (`:206`). Resolution
needs the input source, the xref table, the cache, and the warning sink — all
of which the legacy path reaches through `&mut self`. The legacy path also
holds that `&mut self` **across** nested resolution, which is precisely what
the borrow seam must not do. So `resolve_indirect(&self)` cannot delegate to
it, and "attach now, port later" has no coherent intermediate state.

`flpdf-25kg.3.3`'s own design already said so: the fallible accessors "remain
crate-private until `flpdf-25kg.3.5` attaches the complete qpdf-native resolver
to every document-created handle".

## Ownership shape

`Pdf` holds `Rc<ResolverHandle<R>>`. `ResolverHandle` wraps
`RefCell<ResolverCore<R>>` and implements `DocumentResolver`. Handles receive
`Rc::downgrade(...)` at creation, so `.3.3`'s `Weak` design stands unchanged and
`Pdf<R>` keeps its by-value public shape.

`ResolverCore` holds exactly what qpdf's resolver touches. Enumerated from
`QPDF::resolve`, `QPDF::readObjectAtOffset`, and `QPDF::resolveObjectsInStream`:

| qpdf member | flpdf |
| --- | --- |
| `m->file` | the input source |
| `m->xref_table` | `source_xref_entries` |
| `m->obj_cache` | canonical `ObjectRef → ObjectHandle` cache (new) |
| `m->resolving` | in-progress set (new) |
| `m->resolved_object_streams` | resolved-ObjStm set (new) |
| `m->attempt_recovery` | recovery policy |

Plus the warning sink, and the encryption parameters needed to build the
*string* decrypter the parser uses (`m->encp`, `QPDF.cc:1338`).

### The warning sink reshapes `Pdf::repair_diagnostics`

**Found while landing the in-progress guard; this document did not anticipate
it.** The sink (`m->warnings`, `QPDF.hh:1475`) has to live in `ResolverCore`
because `QPDF::resolve` warns — on a resolution loop (`QPDF.cc:1710`) and on a
damaged object (`:1738`, `:1740`) — and `resolve_indirect` reaches its document
through a `Weak`, never a `&mut Pdf`. Once it is behind the `RefCell`,
`Pdf::repair_diagnostics(&self) -> &Diagnostics` cannot stand: a reference
cannot be handed out from a `RefCell`.

**It returns an owned `Diagnostics` snapshot.** The alternative,
`Ref<'_, Diagnostics>`, avoids the copy but leaks `std::cell::Ref` into the
public API and lets a caller holding one across a resolving call hit a
`BorrowMutError` at run time. The copy is cheap — the collection is empty for a
document that opened cleanly — and both options cost the same call-site churn.

Measured, not estimated: 11 call sites break, not the ~90 that call the method.
Ten are `let x = pdf.repair_diagnostics().entries();` (rustc E0716, the snapshot
is a temporary freed at the end of the statement), fixed by binding the
snapshot first. The eleventh is a different class — a test helper returning
`Vec<&str>` borrowed from the temporary (E0515), fixed by returning owned
`String`s. `flpdf-cli` needs no change at all.

**Do not split the sink in two.** A resolver-local second collection was
considered and rejected: qpdf keeps exactly one `m->warnings`, and warning
*order* across the resolver and the `&mut Pdf` helper walks is already
load-bearing — prepending instead of appending fails seven pre-existing tests
in `nntree`, `linearization::plan`, and `reader` on top of the resolver's own.

**Stream decryption is not in the resolver.** `readObjectAtOffset` is 156 lines
and contains no decryption; qpdf decrypts streams at pipe time
(`decryptStream`, `QPDF.cc:2491`). flpdf's legacy `resolve_to_cache` decrypts
streams during resolution — that placement is legacy and does not carry over.
The pipe-time side is what `flpdf-25kg.3.4`'s native decode entry points serve.

Everything else stays on `Pdf`: `version`, `trailer`, `startxref`,
`foreign_object_maps`, dirty tracking, and every field already carrying a
`qpdf-cutover-delete(flpdf-25kg.3.3)` marker.

## Borrow discipline: use qpdf's seam, do not invent one

Resolution is re-entrant on both sides. qpdf guards it with `m->resolving` and
`ResolveRecorder` (`QPDF.hh:980-996`) because "an object references itself
directly or indirectly in some key that has to be resolved during object
parsing, such as stream length" (`QPDF.cc:1706-1712`).

Streaming from the input source is deliberate — flpdf follows qpdf rather than
parsing from an owned window. That means the input position *is* disturbed by
nested resolution, and qpdf says so at the one place it happens
(`QPDF::readStream`, `QPDF.cc:1360-1398`):

```cpp
// Must get offset before accessing any additional objects since resolving a previously
// unresolved indirect object will change file position.
qpdf_offset_t stream_offset = m->file->tell();
...
auto length_obj = object.getKey("/Length");   // may re-enter resolve
...
m->file->seek(stream_offset, SEEK_SET);       // restore explicitly
```

qpdf does not hold a stable read position across the recursion; it saves and
restores around it. The Rust port drops the `RefCell` borrow at exactly that
point:

1. **short borrow** — capture `stream_offset`
2. **no borrow** — resolve `/Length`; nested `resolve_indirect` re-enters safely
3. **short borrow** — seek back to `stream_offset`, read to `endstream`

`QPDF::resolve` takes the same shape: short borrow to test `isUnresolved`, apply
the cycle guard, insert into `resolving` and read the xref entry; no borrow
while `readObjectAtOffset`/`resolveObjectsInStream` run; short borrow to
`updateCache` and drop the guard. `ResolveRecorder`'s counterpart is a `Drop`
guard, so the in-progress mark is removed on unwind as well as on return.

**Verified: `/Length` is the only parse-time re-entry seam in qpdf.** The other
`file->tell()` sites are xref parsing and recovery, and `QPDF.cc:2451`'s
`/Length` test is object traversal, not resolution. If flpdf's parser grows a
second seam, the fix is to remove it, not to bracket it — a seam qpdf does not
have is a divergence.

## The `'static` bound, and what it does to `open_mem`

**Found during the first attempt at the attach; an earlier revision of this
document did not anticipate it.** Attaching a resolver forces `R: 'static` on
`Pdf<R>`. The chain is short and has no escape:

1. `ObjectHandle` carries no lifetime parameter (`flpdf-25kg.3.3`'s design).
2. Its slot holds `Option<Weak<dyn DocumentResolver>>`, and a trait object in
   that position defaults to `dyn DocumentResolver + 'static`.
3. So the resolver's concrete type must be `'static`, and it owns the input
   source — hence `R: 'static`.

A lifetime-free handle cannot safely reference borrowed data. That is a
property of the language, not a choice: the escapes are `unsafe`, which
`crates/flpdf/src/lib.rs:83` forbids outright with `#![forbid(unsafe_code)]`,
or owning the data. An earlier revision claimed an id-keyed side table could
avoid the bound; that is wrong — such a registry still stores
`Rc<dyn DocumentResolver>` and meets the same default.

**The consequence lands on `Pdf::open_mem`,** which takes `&[u8]` and yields
`Pdf<Cursor<&'a [u8]>>` — not `'static`. This is a parity question, not just an
ergonomic one: qpdf's `processMemoryFile` (`libqpdf/QPDF.cc:259-268`) builds a
`BufferInputSource` over `Buffer(unsigned char*, size_t)`, whose contract is
"memory is owned by the caller and will not be freed when the Buffer is
destroyed" (`include/qpdf/Buffer.hh:42-45`). **qpdf does not copy the bytes.**
Copying silently inside `open_mem` would regress a public API's memory profile
*and* diverge from the oracle.

**Take `Arc<[u8]>` instead.** `Cursor<Arc<[u8]>>` is `Read + Seek + 'static`
(verified by compiling it), and shared ownership is the safe-Rust analogue of
qpdf's contract: the caller keeps a cheap clone, the document reads the same
allocation, and neither side copies. A caller holding only a slice writes
`Arc::from(slice)` itself, so the copy is visible at the call site instead of
hidden in the library.

`Arc` rather than `Rc` because `Pdf` is not `Send` — and has not been since
`flpdf-25kg.3.3` gave `ObjectHandle` its `Rc<RefCell<..>>` identity, which
`reader.rs`'s `handle_registry` comment already records. (An earlier revision
of this document credited the resolver for that; the resolver's own `Rc` is a
second reason, not the first.) Using the atomic form for the *buffer* still
lets one allocation be shared across threads that each open their own
document.

flpdf is pre-1.0 and does not preserve compatibility for its own sake, so
reshaping this entry point is in scope.

## Alternatives rejected

**qpdf-style back-pointer.** `QPDFObject.cc:10` calls
`QPDF::Resolver::resolve(value->qpdf, og)` through a raw pointer, with teardown
safety from `QPDF::~QPDF` disconnecting every cached object — which
`impl Drop for Pdf` (`reader.rs:351-371`) already mirrors. Structurally immune
to the borrow hazard **and** to the `'static` bound, since a raw pointer carries
no lifetime — but `crates/flpdf/src/lib.rs:83` declares
`#![forbid(unsafe_code)]`, so it is not merely undesirable here, it does not
compile. The id-keyed side table offered as its fallback does not work either:
the registry would hold `Rc<dyn DocumentResolver>` and meet the same `'static`
default the raw pointer avoids.

**Explicit dereference at the helper boundary.** Have helpers take `&mut Pdf`
and pre-resolve. No structural change, but it abandons the ObjectHandle-only
boundary that qpdf's helpers have and that `flpdf-9ng9` needs — the
flpdf-specific detour the standing project rule forbids.

## Land it as a new module, not as edits to `reader.rs`

Write the resolver as `crates/flpdf/src/reader/resolver.rs` and swap `Pdf` over
to it, rather than reshaping the resolution code inside `reader.rs`.

`reader/file_object.rs` is the existing precedent for carving a qpdf
responsibility out of `reader.rs`, and the repo has landed this pattern three
times (`reader/file_object.rs`, `writer/{serialize,plain/*}.rs`,
`tokenizer.rs`). The standing project rule prescribes it directly: implement the
qpdf-equivalent path, mark the old route for deletion, and remove it once dead.

It also buys reviewability. `docs/qpdf-correspondence.md:133` currently maps
`QPDF.cc` (2667 lines) across `reader.rs` + `reader/file_object.rs` + `xref.rs`
+ `object_copy.rs` + `cache.rs` + `ref_chain.rs` and rates it 🔀. A single file
that reads one-to-one against `QPDF::resolve`/`readObjectAtOffset`/`readStream`/
`resolveObjectsInStream` can be checked against the oracle line by line; a diff
buried in 7,774 lines cannot. Update that correspondence row as part of the
change.

### What the swap covers, and what it does not

The swap is the **resolver implementation only**:

- `reader/resolver.rs` becomes the single resolution path. `Pdf` calls nothing
  else to resolve an object.
- The legacy APIs (`resolve_borrowed`, `resolve_object_handle`, the terminal
  variants) are **re-hosted on top of it, not deleted yet**. This is not a
  bridge into the new resolver — the dependency runs the other way, and
  `legacy_materialized_memo` already works this way today: its doc says it is
  "populated lazily by `Pdf::resolve_borrowed`" and invalidated by
  `set_object`/`delete_object` "so the next resolve re-derives from the updated
  handle". One source of truth, two views — that property must survive the swap.
- The ~344 `resolve_borrowed` call sites, 117 `resolve_object_handle`, 36
  terminal, and 32 `ref_chain` files are **not touched**.

**The end state is total removal of the legacy APIs** — they are delete-targets,
not a permanent layer. That removal is the existing consumer-migration program
(`flpdf-egzr.3.2`, ending at `flpdf-egzr.3.2.8`, "Remove legacy Object route,
rename handle API, migrate tests"). Do not read the interim layering as the
destination.

Pulling consumer migration into this change instead would put 344 call sites and
32 files into one PR, which is not reviewable.

## Scope boundary

`.3.5` builds the canonical resolver and makes the legacy routes *deletable*.
It does not delete them: `resolve_borrowed` alone has 344 call sites outside
`reader.rs`, `resolve_object_handle` 117, the terminal APIs 36, and `ref_chain`
appears in 32 files. Removing those is the existing consumer-migration program
`flpdf-egzr.3.2` and its subtasks, ending at `flpdf-egzr.3.2.8` ("Remove legacy
Object route, rename handle API, migrate tests").

Check `flpdf-egzr.3.2.10` ("Migrate reader/xref core to ObjectHandle") for
overlap before starting; it is adjacent to this issue's territory.

## Acceptance criteria

Beyond `.3.5`'s existing criteria:

1. A self-referential `/Length` regression proves nested resolution does not
   break the borrow discipline. This is the test that fails if a future edit
   holds a borrow across the seam.
2. `/AP /N` as an indirect stream resolves through nested `try_dereference`
   (inherited from `flpdf-k8ln`).
3. The `Drop` guard removes the in-progress mark on unwind, not only on return.
4. The resolver cannot outlive its `Pdf`.

Note that criterion 2 passing makes `flpdf-nrp3` observable in production for
the first time: a handle disconnected by `Pdf::drop` reads as null, so a
disconnected `/Filter` is seen as absent where qpdf would reject it. Decide
whether to fix that here or keep it separate.

## Corrections made while reaching this design

Recorded because both were wrong turns that cost time and would recur.

- **Measuring the legacy path.** An initial estimate sized the phase split at
  "35 `&mut self` methods, 60 files" by tracing `resolve_to_cache` →
  `read_object_at` → `resolve_pending_stream_length` → `resolve_borrowed`. That
  chain is the route `.3.5` deletes. The canonical port is greenfield for the
  resolution core, and the legacy surface is the migration program's problem.
- **Treating an flpdf-ism as a constraint.** A draft argued the design was safe
  because flpdf reads a bounded window into an owned buffer and parses from it,
  so the reader borrow never spans the parse. That is true of flpdf today and
  is not what qpdf does. Building on it would have entrenched a divergence;
  qpdf's save/restore seam is the thing to port.
