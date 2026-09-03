# ObjectHandle cursor identity and end-state parity

**Issue:** `flpdf-syrr`

## Goal

Make `ArrayItemCursor` and `DictItemCursor` preserve the canonical identity of
the child returned at the current position and distinguish a non-end missing
dictionary entry from the end sentinel, matching qpdf 11.9.0.

## qpdf oracle

Pinned qpdf 11.9.0 assigns the array iterator's `ivalue` from
`getArrayItem(item_number)` (`libqpdf/QPDFObjectHandle.cc:2508-2542`).
`QPDFObjectHandle::isSameObjectAs` compares the underlying shared object
(`libqpdf/QPDFObjectHandle.cc:224-227`), so a value copied from `operator*`
shares identity with the array child. A copied value remains that child when
the iterator later moves; only a C++ reference such as qpdf's
`auto& i_value = *i` observes subsequent assignments to the iterator's own
`ivalue` member (`qpdf/test_driver.cc:1418-1434`).

The dictionary iterator snapshots visible keys in `Members::keys`
(`include/qpdf/QPDFObjectHandle.hh:1485-1508`). For a key position it assigns
`getKey(key)`, while only the key-set end position receives a default
uninitialized handle (`libqpdf/QPDFObjectHandle.cc:2452-2461`). qpdf's
`getKey` returns an initialized null for a missing key or a non-dictionary
receiver (`libqpdf/QPDFObjectHandle.cc:978-989`).

A live probe compiled against the pinned headers and the installed qpdf
11.9.0 library observed:

```text
array_identity=1
array_copy_value=11
array_next_value=22
array_ref_value=22
array_ref_end_initialized=0
dict_removed_is_end=0
dict_removed_initialized=1
dict_removed_null=1
dict_end_initialized=0
```

## Design

`ArrayItemCursor` and `DictItemCursor` retain only their live container,
position, and (for dictionaries) the visible-key snapshot. They no longer
retain a persistent proxy `ObjectHandle` or call `rebind_cursor_value`.

`ArrayItemCursor::current()` returns the actual child handle stored at the
current valid index, which is an `Rc` clone of the child and therefore shares
canonical outer identity. At the cursor's end it returns a new uninitialized
handle. `next` and `previous` update only the position; a previously returned
handle remains the value copied at the time of `current()`, as required by the
Rust value-returning API.

`DictItemCursor::current()` returns the snapshotted key and calls the
container's existing `get_key` for the value at every non-end position. This
preserves the key's child identity when present and produces an initialized
contextual null when the key was removed or the live receiver is no longer a
dictionary. At the key snapshot's end it returns an empty key and a new
uninitialized handle. The key snapshot remains unchanged and no whole-map
clone is introduced per step.

The safe Rust API continues to return `ObjectHandle`/`DictItem` by value; it
does not attempt to expose qpdf's borrowed `auto&` iterator reference. The
qtest driver will therefore keep a copied value stable and read a fresh
`current()` value when it needs to inspect the iterator's later position.

## Consumers and documentation

Update the qtest type-check consumer's stale assertions and comments so they
reflect qpdf's actual C++ reference use and flpdf's safe value-returning
surface. Update the historical type-check design/plan text and the qpdf
correspondence annotation so no document claims that a copied Rust handle
follows iterator movement.

No writer, NameTree/NumberTree implementation, public method signature, legacy
bridge, sentinel substitution, or qpdf-deviation marker is added.

## Testing

Extend the ObjectHandle cursor tests to prove:

- an array `current()` value is `is_same_object_as` the corresponding child;
- the copied array value remains unchanged after `next` and at end;
- a dictionary value has canonical identity while present;
- removing a snapshotted key leaves `is_end() == false` and returns an
  initialized null;
- replacing the live dictionary with a scalar also returns an initialized null;
- only the actual end position returns an uninitialized value.

Run the qtest `type-checks.test` consumer after updating its assertions, then
the focused cursor tests and full workspace gates. The implementation must be
written only after the new regression tests have produced the expected RED
failures, and the same tests must produce GREEN after the proxy is removed.
