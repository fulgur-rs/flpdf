use std::cell::{Cell, RefCell};
use std::rc::Rc;

use flpdf::json::{Json, JsonHandler};

#[test]
fn dictionary_handler_uses_exact_key_then_unknown_key_fallback() {
    let seen = Rc::new(RefCell::new(Vec::new()));
    let exact = JsonHandler::new();
    exact.add_number_handler({
        let seen = seen.clone();
        move |path, number| {
            seen.borrow_mut().push(format!(
                "{}={}",
                String::from_utf8_lossy(path),
                String::from_utf8_lossy(number)
            ));
        }
    });
    let fallback = JsonHandler::new();
    fallback.add_any_handler({
        let seen = seen.clone();
        move |path, _| {
            seen.borrow_mut()
                .push(format!("fallback:{}", String::from_utf8_lossy(path)));
        }
    });

    let root = JsonHandler::new();
    root.add_dictionary_handlers(|_, _| {}, |_| {});
    root.add_dictionary_key_handler(b"known", exact);
    root.add_fallback_dictionary_handler(fallback);
    root.handle(b".", Json::parse(br#"{"known":1,"other":null}"#).unwrap())
        .unwrap();

    assert_eq!(&*seen.borrow(), &[".known=1", "fallback:.other"]);
}

#[test]
fn dictionary_dispatch_rereads_registration_before_each_item() {
    let seen = Rc::new(RefCell::new(Vec::new()));
    let root = JsonHandler::new();
    root.add_dictionary_handlers(|_, _| {}, |_| {});

    let replacement = JsonHandler::new();
    replacement.add_number_handler({
        let seen = seen.clone();
        move |path, number| {
            seen.borrow_mut().push(format!(
                "{}={}",
                String::from_utf8_lossy(path),
                String::from_utf8_lossy(number)
            ));
        }
    });
    let original = JsonHandler::new();
    original.add_any_handler(|_, _| {
        panic!("stale registration was used");
    });
    let first = JsonHandler::new();
    first.add_number_handler({
        let root = root.downgrade();
        let replacement = replacement.clone();
        move |_, _| {
            root.upgrade()
                .expect("root handler must be alive during dispatch")
                .add_dictionary_key_handler(b"b", replacement.clone());
        }
    });
    root.add_dictionary_key_handler(b"a", first);
    root.add_dictionary_key_handler(b"b", original);

    root.handle(b".", Json::parse(br#"{"a":1,"b":2}"#).unwrap())
        .unwrap();
    assert_eq!(&*seen.borrow(), &[".b=2"]);
}

#[test]
fn dictionary_dispatch_rereads_fallback_before_each_item() {
    let seen = Rc::new(RefCell::new(Vec::new()));
    let root = JsonHandler::new();
    root.add_dictionary_handlers(|_, _| {}, |_| {});

    let replacement = JsonHandler::new();
    replacement.add_number_handler({
        let seen = seen.clone();
        move |path, number| {
            seen.borrow_mut().push(format!(
                "{}={}",
                String::from_utf8_lossy(path),
                String::from_utf8_lossy(number)
            ));
        }
    });
    let stale = JsonHandler::new();
    stale.add_any_handler(|_, _| {
        panic!("stale fallback was used");
    });
    let first = JsonHandler::new();
    first.add_number_handler({
        let root = root.downgrade();
        let replacement = replacement.clone();
        move |_, _| {
            root.upgrade()
                .expect("root handler must be alive during dispatch")
                .add_fallback_dictionary_handler(replacement.clone());
        }
    });
    root.add_dictionary_key_handler(b"a", first);
    root.add_fallback_dictionary_handler(stale);

    root.handle(b".", Json::parse(br#"{"a":1,"b":2}"#).unwrap())
        .unwrap();
    assert_eq!(&*seen.borrow(), &[".b=2"]);
}

#[test]
fn dictionary_dispatch_rereads_end_handler_after_item_replaces_root_handlers() {
    let seen = Rc::new(RefCell::new(Vec::new()));
    let root = JsonHandler::new();
    root.add_dictionary_handlers(
        {
            let seen = seen.clone();
            move |path, _| {
                seen.borrow_mut()
                    .push(format!("original start:{}", String::from_utf8_lossy(path)));
            }
        },
        {
            let seen = seen.clone();
            move |path| {
                seen.borrow_mut()
                    .push(format!("original end:{}", String::from_utf8_lossy(path)));
            }
        },
    );

    let first = JsonHandler::new();
    first.add_number_handler({
        let root = root.downgrade();
        let seen = seen.clone();
        move |path, number| {
            seen.borrow_mut().push(format!(
                "item:{}={}",
                String::from_utf8_lossy(path),
                String::from_utf8_lossy(number)
            ));
            root.upgrade()
                .expect("root handler must be alive during dispatch")
                .add_dictionary_handlers(|_, _| {}, {
                    let seen = seen.clone();
                    move |path| {
                        seen.borrow_mut()
                            .push(format!("replacement end:{}", String::from_utf8_lossy(path)));
                    }
                });
        }
    });
    root.add_dictionary_key_handler(b"a", first);

    root.handle(b".", Json::parse(br#"{"a":1}"#).unwrap())
        .unwrap();

    assert_eq!(
        &*seen.borrow(),
        &["original start:.", "item:.a=1", "replacement end:."]
    );
}

#[test]
fn dictionary_handler_calls_start_items_and_end_in_order_with_original_value() {
    let seen = Rc::new(RefCell::new(Vec::new()));
    let item = JsonHandler::new();
    item.add_any_handler({
        let seen = seen.clone();
        move |path, value| {
            seen.borrow_mut().push(format!(
                "item:{}={}",
                String::from_utf8_lossy(path),
                String::from_utf8_lossy(&value.unparse().unwrap())
            ));
        }
    });

    let handler = JsonHandler::new();
    handler.add_dictionary_handlers(
        {
            let seen = seen.clone();
            move |path, value| {
                seen.borrow_mut().push(format!(
                    "start:{}={}",
                    String::from_utf8_lossy(path),
                    String::from_utf8_lossy(&value.unparse().unwrap())
                ));
            }
        },
        {
            let seen = seen.clone();
            move |path| {
                seen.borrow_mut()
                    .push(format!("end:{}", String::from_utf8_lossy(path)));
            }
        },
    );
    handler.add_dictionary_key_handler(b"a", item.clone());
    handler.add_dictionary_key_handler(b"b", item);

    handler
        .handle(b".root", Json::parse(br#"{"b":2,"a":1}"#).unwrap())
        .unwrap();

    assert_eq!(
        &*seen.borrow(),
        &[
            "start:.root={\n  \"a\": 1,\n  \"b\": 2\n}",
            "item:.root.a=1",
            "item:.root.b=2",
            "end:.root",
        ]
    );
}

#[test]
fn dictionary_handler_observes_later_member_mutations_and_insertions() {
    let dictionary = Json::parse(br#"{"a":1,"b":2}"#).unwrap();
    let seen = Rc::new(RefCell::new(Vec::new()));

    let first = JsonHandler::new();
    first.add_number_handler({
        let dictionary = dictionary.clone();
        let seen = seen.clone();
        move |path, number| {
            seen.borrow_mut().push(format!(
                "{}={}",
                String::from_utf8_lossy(path),
                String::from_utf8_lossy(number)
            ));
            dictionary
                .add_dictionary_member(b"b", Json::make_int(20))
                .unwrap();
            dictionary
                .add_dictionary_member(b"c", Json::make_int(3))
                .unwrap();
        }
    });
    let later = JsonHandler::new();
    later.add_number_handler({
        let seen = seen.clone();
        move |path, number| {
            seen.borrow_mut().push(format!(
                "{}={}",
                String::from_utf8_lossy(path),
                String::from_utf8_lossy(number)
            ));
        }
    });

    let root = JsonHandler::new();
    root.add_dictionary_handlers(|_, _| {}, |_| {});
    root.add_dictionary_key_handler(b"a", first);
    root.add_dictionary_key_handler(b"b", later.clone());
    root.add_dictionary_key_handler(b"c", later);
    root.handle(b".", dictionary).unwrap();

    assert_eq!(&*seen.borrow(), &[".a=1", ".b=20", ".c=3"]);
}

#[test]
fn typed_scalar_handlers_receive_qpdf_values_and_paths() {
    let seen = Rc::new(RefCell::new(Vec::new()));
    let handler = JsonHandler::new();
    handler.add_null_handler({
        let seen = seen.clone();
        move |path| {
            seen.borrow_mut()
                .push(format!("{}=null", String::from_utf8_lossy(path)));
        }
    });
    handler.add_string_handler({
        let seen = seen.clone();
        move |path, string| {
            seen.borrow_mut().push(format!(
                "{}=string:{}",
                String::from_utf8_lossy(path),
                String::from_utf8_lossy(string)
            ));
        }
    });
    handler.add_number_handler({
        let seen = seen.clone();
        move |path, number| {
            seen.borrow_mut().push(format!(
                "{}=number:{}",
                String::from_utf8_lossy(path),
                String::from_utf8_lossy(number)
            ));
        }
    });
    handler.add_bool_handler({
        let seen = seen.clone();
        move |path, boolean| {
            seen.borrow_mut()
                .push(format!("{}=bool:{boolean}", String::from_utf8_lossy(path)));
        }
    });

    for (path, input) in [
        (b".null".as_slice(), b"null".as_slice()),
        (b".string".as_slice(), br#""potato""#.as_slice()),
        (b".number".as_slice(), b"2.1e5".as_slice()),
        (b".boolean".as_slice(), b"true".as_slice()),
    ] {
        handler.handle(path, Json::parse(input).unwrap()).unwrap();
    }

    assert_eq!(
        &*seen.borrow(),
        &[
            ".null=null",
            ".string=string:potato",
            ".number=number:2.1e5",
            ".boolean=bool:true",
        ]
    );
}

#[test]
fn any_handler_preempts_typed_and_container_handlers() {
    let seen = Rc::new(RefCell::new(Vec::new()));
    let handler = JsonHandler::new();
    handler.add_any_handler({
        let seen = seen.clone();
        move |path, value| {
            seen.borrow_mut().push(format!(
                "any:{}={}",
                String::from_utf8_lossy(path),
                String::from_utf8_lossy(&value.unparse().unwrap())
            ));
        }
    });
    handler.add_null_handler({
        let seen = seen.clone();
        move |_| seen.borrow_mut().push("null".into())
    });
    handler.add_dictionary_handlers(
        {
            let seen = seen.clone();
            move |_, _| seen.borrow_mut().push("dictionary start".into())
        },
        {
            let seen = seen.clone();
            move |_| seen.borrow_mut().push("dictionary end".into())
        },
    );

    handler
        .handle(b".", Json::parse(br#"{"a":null}"#).unwrap())
        .unwrap();

    assert_eq!(
        &*seen.borrow(),
        &[concat!("any:.={\n", "  \"a\": null\n", "}")]
    );
}

#[test]
fn arrays_use_indexed_paths_then_general_fallback_handles_a_scalar() {
    let seen = Rc::new(RefCell::new(Vec::new()));
    let item = JsonHandler::new();
    item.add_string_handler({
        let seen = seen.clone();
        move |path, value| {
            seen.borrow_mut().push(format!(
                "{}={}",
                String::from_utf8_lossy(path),
                String::from_utf8_lossy(value)
            ));
        }
    });
    let scalar = JsonHandler::new();
    scalar.add_string_handler({
        let seen = seen.clone();
        move |path, value| {
            seen.borrow_mut().push(format!(
                "fallback:{}={}",
                String::from_utf8_lossy(path),
                String::from_utf8_lossy(value)
            ));
        }
    });

    let handler = JsonHandler::new();
    handler.add_array_handlers(
        {
            let seen = seen.clone();
            move |path, _| {
                seen.borrow_mut()
                    .push(format!("start:{}", String::from_utf8_lossy(path)));
            }
        },
        {
            let seen = seen.clone();
            move |path| {
                seen.borrow_mut()
                    .push(format!("end:{}", String::from_utf8_lossy(path)));
            }
        },
        item,
    );
    handler.add_fallback_handler(scalar);

    handler
        .handle(b".items", Json::parse(br#"["x","y"]"#).unwrap())
        .unwrap();
    handler
        .handle(b".items", Json::parse(br#""not-array""#).unwrap())
        .unwrap();

    assert_eq!(
        &*seen.borrow(),
        &[
            "start:.items",
            ".items[0]=x",
            ".items[1]=y",
            "end:.items",
            "fallback:.items=not-array",
        ]
    );
}

#[test]
fn array_dispatch_rereads_item_and_end_handlers_after_first_item_replaces_root_handlers() {
    let seen = Rc::new(RefCell::new(Vec::new()));
    let root = JsonHandler::new();

    let replacement = JsonHandler::new();
    replacement.add_number_handler({
        let seen = seen.clone();
        move |path, number| {
            seen.borrow_mut().push(format!(
                "replacement item:{}={}",
                String::from_utf8_lossy(path),
                String::from_utf8_lossy(number)
            ));
        }
    });
    let first = JsonHandler::new();
    first.add_number_handler({
        let root = root.downgrade();
        let replacement = replacement.clone();
        let seen = seen.clone();
        move |path, number| {
            seen.borrow_mut().push(format!(
                "first item:{}={}",
                String::from_utf8_lossy(path),
                String::from_utf8_lossy(number)
            ));
            root.upgrade()
                .expect("root handler must be alive during dispatch")
                .add_array_handlers(
                    |_, _| {},
                    {
                        let seen = seen.clone();
                        move |path| {
                            seen.borrow_mut()
                                .push(format!("replacement end:{}", String::from_utf8_lossy(path)));
                        }
                    },
                    replacement.clone(),
                );
        }
    });
    root.add_array_handlers(
        {
            let seen = seen.clone();
            move |path, _| {
                seen.borrow_mut()
                    .push(format!("original start:{}", String::from_utf8_lossy(path)));
            }
        },
        {
            let seen = seen.clone();
            move |path| {
                seen.borrow_mut()
                    .push(format!("original end:{}", String::from_utf8_lossy(path)));
            }
        },
        first,
    );

    root.handle(b".items", Json::parse(br#"[1,2]"#).unwrap())
        .unwrap();

    assert_eq!(
        &*seen.borrow(),
        &[
            "original start:.items",
            "first item:.items[0]=1",
            "replacement item:.items[1]=2",
            "replacement end:.items",
        ]
    );
}

#[test]
fn nested_dictionary_paths_push_encoded_keys_and_pop_for_siblings() {
    let seen = Rc::new(RefCell::new(Vec::new()));
    let leaf = JsonHandler::new();
    leaf.add_number_handler({
        let seen = seen.clone();
        move |path, _| {
            seen.borrow_mut()
                .push(String::from_utf8_lossy(path).into_owned());
        }
    });
    let nested = JsonHandler::new();
    nested.add_dictionary_handlers(|_, _| {}, |_| {});
    nested.add_dictionary_key_handler(b"line\\n", leaf.clone());
    nested.add_dictionary_key_handler(b"plain", leaf);

    let root = JsonHandler::new();
    root.add_dictionary_handlers(|_, _| {}, |_| {});
    root.add_dictionary_key_handler(b"outer", nested);
    root.handle(
        b"root",
        Json::parse(b"{\"outer\":{\"line\\n\":1,\"plain\":2}}").unwrap(),
    )
    .unwrap();

    assert_eq!(&*seen.borrow(), &["root.outer.line\\n", "root.outer.plain"]);
}

#[test]
fn unhandled_value_reports_the_qpdf_method_specific_path() {
    let handler = JsonHandler::new();

    let error = handler
        .handle(b".", Json::make_string(b"oops"))
        .unwrap_err();

    assert_eq!(
        error.to_string(),
        "JSON handler: value at . is not of expected type"
    );
    assert_eq!(
        error,
        flpdf::json::JsonHandlerError(flpdf::json::JsonMessage::from(
            "JSON handler: value at . is not of expected type",
        ))
    );
}

#[test]
fn handler_errors_preserve_non_utf8_key_and_path_bytes() {
    let handler = JsonHandler::new();
    handler.add_dictionary_handlers(|_, _| {}, |_| {});

    let dictionary = Json::make_dictionary();
    dictionary
        .add_dictionary_member(b"\xff", Json::make_null())
        .unwrap();
    let error = handler.handle(b".\x80", dictionary).unwrap_err();

    assert_eq!(
        error.0.as_bytes(),
        b"JSON handler found unexpected key \xff in object at .\x80"
    );

    let scalar = JsonHandler::new();
    let error = scalar.handle(b".\xff", Json::make_null()).unwrap_err();
    assert_eq!(
        error.0.as_bytes(),
        b"JSON handler: value at .\xff is not of expected type"
    );
}

#[test]
fn unconfigured_type_handlers_reject_null_bool_and_containers() {
    let handler = JsonHandler::new();
    handler.add_string_handler(|_, _| {});

    for (path, value, expected) in [
        (
            b".null".as_slice(),
            Json::make_null(),
            "JSON handler: value at .null is not of expected type",
        ),
        (
            b".dictionary".as_slice(),
            Json::make_dictionary(),
            "JSON handler: value at .dictionary is not of expected type",
        ),
        (
            b".array".as_slice(),
            Json::make_array(),
            "JSON handler: value at .array is not of expected type",
        ),
    ] {
        assert_eq!(
            handler.handle(path, value).unwrap_err().to_string(),
            expected
        );
    }

    let error = handler.handle(b".bool", Json::make_bool(true)).unwrap_err();
    assert_eq!(
        error.0.as_bytes(),
        b"JSON handler: value at .bool is not of expected type"
    );
}

#[test]
fn unexpected_nested_key_reports_its_object_path_and_skips_end_handlers() {
    let ends = Rc::new(RefCell::new(Vec::new()));
    let nested = JsonHandler::new();
    nested.add_dictionary_handlers(|_, _| {}, {
        let ends = ends.clone();
        move |path| {
            ends.borrow_mut()
                .push(String::from_utf8_lossy(path).into_owned());
        }
    });

    let root = JsonHandler::new();
    root.add_dictionary_handlers(|_, _| {}, {
        let ends = ends.clone();
        move |path| {
            ends.borrow_mut()
                .push(String::from_utf8_lossy(path).into_owned());
        }
    });
    root.add_dictionary_key_handler(b"known", nested);

    let error = root
        .handle(
            b".",
            Json::parse(br#"{"known":{"x":"y","z":"must be skipped"}}"#).unwrap(),
        )
        .unwrap_err();

    assert_eq!(
        error.to_string(),
        "JSON handler found unexpected key x in object at .known"
    );
    assert!(ends.borrow().is_empty());
}

#[test]
fn self_referential_dictionary_fallback_handles_finite_recursive_json() {
    let seen = Rc::new(RefCell::new(Vec::new()));
    let handler = JsonHandler::new();
    handler.add_dictionary_handlers(
        {
            let seen = seen.clone();
            move |path, _| {
                seen.borrow_mut()
                    .push(format!("start:{}", String::from_utf8_lossy(path)));
            }
        },
        {
            let seen = seen.clone();
            move |path| {
                seen.borrow_mut()
                    .push(format!("end:{}", String::from_utf8_lossy(path)));
            }
        },
    );
    handler.add_string_handler({
        let seen = seen.clone();
        move |path, value| {
            seen.borrow_mut().push(format!(
                "{}={}",
                String::from_utf8_lossy(path),
                String::from_utf8_lossy(value)
            ));
        }
    });
    handler.add_fallback_dictionary_handler(handler.clone());

    handler
        .handle(b".", Json::parse(br#"{"a":{"b":"done"}}"#).unwrap())
        .unwrap();

    assert_eq!(
        &*seen.borrow(),
        &["start:.", "start:.a", ".a.b=done", "end:.a", "end:.",]
    );
}

#[test]
fn active_recursive_fallback_refreshes_the_root_handlers_after_dictionary_start() {
    let seen = Rc::new(RefCell::new(Vec::new()));
    let root = JsonHandler::new();
    root.add_dictionary_handlers(
        {
            let root = root.downgrade();
            let seen = seen.clone();
            move |path, _| {
                seen.borrow_mut()
                    .push(format!("start:{}", String::from_utf8_lossy(path)));
                root.upgrade()
                    .expect("root handler must be alive during dispatch")
                    .add_number_handler({
                        let seen = seen.clone();
                        move |path, number| {
                            seen.borrow_mut().push(format!(
                                "{}={}",
                                String::from_utf8_lossy(path),
                                String::from_utf8_lossy(number)
                            ));
                        }
                    });
            }
        },
        |_| {},
    );
    root.add_fallback_dictionary_handler(root.clone());

    root.handle(b".", Json::parse(br#"{"a":1}"#).unwrap())
        .unwrap();

    assert_eq!(&*seen.borrow(), &["start:.", ".a=1"]);
}

#[test]
fn mutually_recursive_dictionary_fallbacks_handle_a_finite_cycle() {
    let seen = Rc::new(RefCell::new(Vec::new()));
    let first = JsonHandler::new();
    let second = JsonHandler::new();
    first.add_dictionary_handlers(
        {
            let seen = seen.clone();
            move |path, _| {
                seen.borrow_mut()
                    .push(format!("first start:{}", String::from_utf8_lossy(path)));
            }
        },
        {
            let seen = seen.clone();
            move |path| {
                seen.borrow_mut()
                    .push(format!("first end:{}", String::from_utf8_lossy(path)));
            }
        },
    );
    first.add_string_handler({
        let seen = seen.clone();
        move |path, value| {
            seen.borrow_mut().push(format!(
                "{}={}",
                String::from_utf8_lossy(path),
                String::from_utf8_lossy(value)
            ));
        }
    });
    second.add_dictionary_handlers(
        {
            let seen = seen.clone();
            move |path, _| {
                seen.borrow_mut()
                    .push(format!("second start:{}", String::from_utf8_lossy(path)));
            }
        },
        {
            let seen = seen.clone();
            move |path| {
                seen.borrow_mut()
                    .push(format!("second end:{}", String::from_utf8_lossy(path)));
            }
        },
    );
    first.add_fallback_dictionary_handler(second.clone());
    second.add_fallback_dictionary_handler(first.clone());
    drop(second);

    first
        .handle(
            b".",
            Json::parse(br#"{"first":{"second":"done"}}"#).unwrap(),
        )
        .unwrap();

    assert_eq!(
        &*seen.borrow(),
        &[
            "first start:.",
            "second start:.first",
            ".first.second=done",
            "second end:.first",
            "first end:.",
        ]
    );
}

#[test]
fn recursive_registration_cycles_release_handlers_and_callbacks() {
    let drops = Rc::new(Cell::new(0));

    let handler = JsonHandler::new();
    let handler_weak = handler.downgrade();
    handler.add_any_handler({
        let probe = DropProbe(drops.clone());
        move |_, _| {
            let _ = &probe;
        }
    });
    handler.add_fallback_dictionary_handler(handler.clone());
    drop(handler);

    assert!(handler_weak.upgrade().is_none());
    assert_eq!(drops.get(), 1);

    let first = JsonHandler::new();
    let second = JsonHandler::new();
    let first_weak = first.downgrade();
    let second_weak = second.downgrade();
    first.add_any_handler({
        let probe = DropProbe(drops.clone());
        move |_, _| {
            let _ = &probe;
        }
    });
    second.add_any_handler({
        let probe = DropProbe(drops.clone());
        move |_, _| {
            let _ = &probe;
        }
    });
    first.add_fallback_dictionary_handler(second.clone());
    second.add_fallback_dictionary_handler(first.clone());
    drop(first);
    drop(second);

    assert!(first_weak.upgrade().is_none());
    assert!(second_weak.upgrade().is_none());
    assert_eq!(drops.get(), 3);

    let first = JsonHandler::new();
    let second = JsonHandler::new();
    let observer = JsonHandler::new();
    let first_weak = first.downgrade();
    let second_weak = second.downgrade();
    first.add_dictionary_key_handler(b"next", second.clone());
    second.add_fallback_handler(first.clone());
    observer.add_fallback_handler(first.clone());
    drop(first);
    drop(second);
    drop(observer);
    assert!(first_weak.upgrade().is_none());
    assert!(second_weak.upgrade().is_none());

    let first = JsonHandler::new();
    let second = JsonHandler::new();
    let first_weak = first.downgrade();
    let second_weak = second.downgrade();
    first.add_array_handlers(|_, _| {}, |_| {}, second.clone());
    second.add_fallback_handler(first.clone());
    drop(first);
    drop(second);
    assert!(first_weak.upgrade().is_none());
    assert!(second_weak.upgrade().is_none());

    let owner = JsonHandler::new();
    let child = JsonHandler::new();
    let left = JsonHandler::new();
    let right = JsonHandler::new();
    let owner_weak = owner.downgrade();
    let child_weak = child.downgrade();
    let left_weak = left.downgrade();
    let right_weak = right.downgrade();
    child.add_dictionary_key_handler(b"left", left.clone());
    child.add_dictionary_key_handler(b"right", right.clone());
    left.add_fallback_handler(owner.clone());
    right.add_fallback_handler(owner.clone());
    owner.add_fallback_handler(child.clone());
    drop(owner);
    drop(child);
    drop(left);
    drop(right);
    assert!(owner_weak.upgrade().is_none());
    assert!(child_weak.upgrade().is_none());
    assert!(left_weak.upgrade().is_none());
    assert!(right_weak.upgrade().is_none());
}

#[test]
fn cycle_closing_back_edge_keeps_entry_target_strong() {
    let seen = Rc::new(RefCell::new(Vec::new()));
    let first = JsonHandler::new();
    let second = JsonHandler::new();
    let second_weak = second.downgrade();
    let terminal = JsonHandler::new();
    terminal.add_number_handler({
        let seen = seen.clone();
        move |path, number| {
            seen.borrow_mut().push(format!(
                "{}={}",
                String::from_utf8_lossy(path),
                String::from_utf8_lossy(number)
            ));
        }
    });
    first.add_fallback_handler(second.clone());
    second.add_fallback_handler(first.clone());
    drop(second);
    assert!(second_weak.upgrade().is_some());

    first.add_fallback_handler(terminal);
    assert!(second_weak.upgrade().is_none());
    first.handle(b".value", Json::make_int(1)).unwrap();
    assert_eq!(&*seen.borrow(), &[".value=1"]);
}

#[test]
fn handler_configuration_after_registration_is_used_at_dispatch_time() {
    let seen = Rc::new(RefCell::new(Vec::new()));
    let child = JsonHandler::new();
    let root = JsonHandler::new();
    root.add_dictionary_handlers(|_, _| {}, |_| {});
    root.add_dictionary_key_handler(b"late", child.clone());

    child.add_string_handler({
        let seen = seen.clone();
        move |path, value| {
            seen.borrow_mut().push(format!(
                "{}={}",
                String::from_utf8_lossy(path),
                String::from_utf8_lossy(value)
            ));
        }
    });

    root.handle(b".", Json::parse(br#"{"late":"configured"}"#).unwrap())
        .unwrap();

    assert_eq!(&*seen.borrow(), &[".late=configured"]);
}

#[test]
fn live_handle_allows_callback_reentry_into_a_different_nested_callback() {
    let seen = Rc::new(RefCell::new(Vec::new()));
    let handler = JsonHandler::new();
    let weak = handler.downgrade();
    handler.add_null_handler({
        let seen = seen.clone();
        move |path| {
            seen.borrow_mut()
                .push(format!("null:{}", String::from_utf8_lossy(path)));
        }
    });
    handler.add_string_handler({
        let handler = handler.downgrade();
        let seen = seen.clone();
        move |path, value| {
            seen.borrow_mut().push(format!(
                "string:{}={}",
                String::from_utf8_lossy(path),
                String::from_utf8_lossy(value)
            ));
            handler
                .upgrade()
                .expect("handler must be alive during callback reentry")
                .handle(b".nested", Json::make_null())
                .unwrap();
        }
    });

    handler.handle(b".", Json::make_string(b"outer")).unwrap();

    assert_eq!(&*seen.borrow(), &["string:.=outer", "null:.nested"]);

    drop(handler);
    assert!(weak.upgrade().is_none());
}

#[test]
fn same_active_callback_can_reenter_itself_synchronously() {
    let seen = Rc::new(RefCell::new(Vec::new()));
    let depth = Rc::new(Cell::new(0));
    let handler = JsonHandler::new();
    handler.add_string_handler({
        let weak = handler.downgrade();
        let seen = seen.clone();
        let depth = depth.clone();
        move |path, value| {
            seen.borrow_mut().push((path.to_vec(), value.to_vec()));
            if depth.replace(1) == 0 {
                weak.upgrade()
                    .expect("handler is alive")
                    .handle(b".nested", Json::make_string(b"inner"))
                    .unwrap();
            }
        }
    });

    handler.handle(b".", Json::make_string(b"outer")).unwrap();

    assert_eq!(
        &*seen.borrow(),
        &[
            (b".".to_vec(), b"outer".to_vec()),
            (b".nested".to_vec(), b"inner".to_vec()),
        ]
    );
}

struct DropProbe(Rc<Cell<usize>>);

impl Drop for DropProbe {
    fn drop(&mut self) {
        self.0.set(self.0.get() + 1);
    }
}

#[test]
fn replacing_an_unselected_target_drops_it_during_dispatch() {
    let drops = Rc::new(Cell::new(0));
    let seen = Rc::new(RefCell::new(Vec::new()));
    let root = JsonHandler::new();
    let stale = JsonHandler::new();
    stale.add_any_handler({
        let probe = DropProbe(drops.clone());
        move |_, _| {
            let _ = &probe;
        }
    });
    let stale_weak = stale.downgrade();
    root.add_dictionary_key_handler(b"b", stale.clone());
    drop(stale);

    let replacement = JsonHandler::new();
    replacement.add_number_handler({
        let seen = seen.clone();
        move |path, number| {
            seen.borrow_mut().push((path.to_vec(), number.to_vec()));
        }
    });
    root.add_dictionary_handlers(
        {
            let weak = root.downgrade();
            let drops = drops.clone();
            let stale_weak = stale_weak.clone();
            move |_, _| {
                weak.upgrade()
                    .expect("root is alive")
                    .add_dictionary_key_handler(b"b", replacement.clone());
                assert_eq!(drops.get(), 1);
                assert!(stale_weak.upgrade().is_none());
            }
        },
        |_| {},
    );

    root.handle(b".", Json::parse(br#"{"b":1}"#).unwrap())
        .unwrap();

    assert_eq!(&*seen.borrow(), &[(b".b".to_vec(), b"1".to_vec())]);
    assert_eq!(drops.get(), 1);
}
