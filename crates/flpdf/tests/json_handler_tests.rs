use std::cell::RefCell;
use std::rc::Rc;

use flpdf::json::{Json, JsonHandler};

#[test]
fn dictionary_handler_uses_exact_key_then_unknown_key_fallback() {
    let seen = Rc::new(RefCell::new(Vec::new()));
    let exact = JsonHandler::shared();
    exact.borrow_mut().add_number_handler({
        let seen = seen.clone();
        move |path, number| {
            seen.borrow_mut().push(format!(
                "{}={}",
                String::from_utf8_lossy(path),
                String::from_utf8_lossy(number)
            ));
        }
    });
    let fallback = JsonHandler::shared();
    fallback.borrow_mut().add_any_handler({
        let seen = seen.clone();
        move |path, _| {
            seen.borrow_mut()
                .push(format!("fallback:{}", String::from_utf8_lossy(path)));
        }
    });

    let mut root = JsonHandler::new();
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
    let root = JsonHandler::shared();
    root.borrow_mut().add_dictionary_handlers(|_, _| {}, |_| {});

    let replacement = JsonHandler::shared();
    replacement.borrow_mut().add_number_handler({
        let seen = seen.clone();
        move |path, number| {
            seen.borrow_mut().push(format!(
                "{}={}",
                String::from_utf8_lossy(path),
                String::from_utf8_lossy(number)
            ));
        }
    });
    let original = JsonHandler::shared();
    original.borrow_mut().add_any_handler(|_, _| {
        panic!("stale registration was used");
    });
    let first = JsonHandler::shared();
    first.borrow_mut().add_number_handler({
        let root = Rc::downgrade(&root);
        let replacement = replacement.clone();
        move |_, _| {
            root.upgrade()
                .expect("root handler must be alive during dispatch")
                .borrow_mut()
                .add_dictionary_key_handler(b"b", replacement.clone());
        }
    });
    root.borrow_mut().add_dictionary_key_handler(b"a", first);
    root.borrow_mut().add_dictionary_key_handler(b"b", original);

    JsonHandler::handle_shared(&root, b".", Json::parse(br#"{"a":1,"b":2}"#).unwrap()).unwrap();
    assert_eq!(&*seen.borrow(), &[".b=2"]);
}

#[test]
fn dictionary_dispatch_rereads_fallback_before_each_item() {
    let seen = Rc::new(RefCell::new(Vec::new()));
    let root = JsonHandler::shared();
    root.borrow_mut().add_dictionary_handlers(|_, _| {}, |_| {});

    let replacement = JsonHandler::shared();
    replacement.borrow_mut().add_number_handler({
        let seen = seen.clone();
        move |path, number| {
            seen.borrow_mut().push(format!(
                "{}={}",
                String::from_utf8_lossy(path),
                String::from_utf8_lossy(number)
            ));
        }
    });
    let stale = JsonHandler::shared();
    stale.borrow_mut().add_any_handler(|_, _| {
        panic!("stale fallback was used");
    });
    let first = JsonHandler::shared();
    first.borrow_mut().add_number_handler({
        let root = Rc::downgrade(&root);
        let replacement = replacement.clone();
        move |_, _| {
            root.upgrade()
                .expect("root handler must be alive during dispatch")
                .borrow_mut()
                .add_fallback_dictionary_handler(replacement.clone());
        }
    });
    root.borrow_mut().add_dictionary_key_handler(b"a", first);
    root.borrow_mut().add_fallback_dictionary_handler(stale);

    JsonHandler::handle_shared(&root, b".", Json::parse(br#"{"a":1,"b":2}"#).unwrap()).unwrap();
    assert_eq!(&*seen.borrow(), &[".b=2"]);
}

#[test]
fn dictionary_dispatch_rereads_end_handler_after_item_replaces_root_handlers() {
    let seen = Rc::new(RefCell::new(Vec::new()));
    let root = JsonHandler::shared();
    root.borrow_mut().add_dictionary_handlers(
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

    let first = JsonHandler::shared();
    first.borrow_mut().add_number_handler({
        let root = Rc::downgrade(&root);
        let seen = seen.clone();
        move |path, number| {
            seen.borrow_mut().push(format!(
                "item:{}={}",
                String::from_utf8_lossy(path),
                String::from_utf8_lossy(number)
            ));
            root.upgrade()
                .expect("root handler must be alive during dispatch")
                .borrow_mut()
                .add_dictionary_handlers(|_, _| {}, {
                    let seen = seen.clone();
                    move |path| {
                        seen.borrow_mut()
                            .push(format!("replacement end:{}", String::from_utf8_lossy(path)));
                    }
                });
        }
    });
    root.borrow_mut().add_dictionary_key_handler(b"a", first);

    JsonHandler::handle_shared(&root, b".", Json::parse(br#"{"a":1}"#).unwrap()).unwrap();

    assert_eq!(
        &*seen.borrow(),
        &["original start:.", "item:.a=1", "replacement end:."]
    );
}

#[test]
fn dictionary_handler_observes_later_member_mutations_and_insertions() {
    let dictionary = Json::parse(br#"{"a":1,"b":2}"#).unwrap();
    let seen = Rc::new(RefCell::new(Vec::new()));

    let first = JsonHandler::shared();
    first.borrow_mut().add_number_handler({
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
    let later = JsonHandler::shared();
    later.borrow_mut().add_number_handler({
        let seen = seen.clone();
        move |path, number| {
            seen.borrow_mut().push(format!(
                "{}={}",
                String::from_utf8_lossy(path),
                String::from_utf8_lossy(number)
            ));
        }
    });

    let mut root = JsonHandler::new();
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
    let mut handler = JsonHandler::new();
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
    let mut handler = JsonHandler::new();
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
    let item = JsonHandler::shared();
    item.borrow_mut().add_string_handler({
        let seen = seen.clone();
        move |path, value| {
            seen.borrow_mut().push(format!(
                "{}={}",
                String::from_utf8_lossy(path),
                String::from_utf8_lossy(value)
            ));
        }
    });
    let scalar = JsonHandler::shared();
    scalar.borrow_mut().add_string_handler({
        let seen = seen.clone();
        move |path, value| {
            seen.borrow_mut().push(format!(
                "fallback:{}={}",
                String::from_utf8_lossy(path),
                String::from_utf8_lossy(value)
            ));
        }
    });

    let mut handler = JsonHandler::new();
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
    let root = JsonHandler::shared();

    let replacement = JsonHandler::shared();
    replacement.borrow_mut().add_number_handler({
        let seen = seen.clone();
        move |path, number| {
            seen.borrow_mut().push(format!(
                "replacement item:{}={}",
                String::from_utf8_lossy(path),
                String::from_utf8_lossy(number)
            ));
        }
    });
    let first = JsonHandler::shared();
    first.borrow_mut().add_number_handler({
        let root = Rc::downgrade(&root);
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
                .borrow_mut()
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
    root.borrow_mut().add_array_handlers(
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

    JsonHandler::handle_shared(&root, b".items", Json::parse(br#"[1,2]"#).unwrap()).unwrap();

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
    let leaf = JsonHandler::shared();
    leaf.borrow_mut().add_number_handler({
        let seen = seen.clone();
        move |path, _| {
            seen.borrow_mut()
                .push(String::from_utf8_lossy(path).into_owned());
        }
    });
    let nested = JsonHandler::shared();
    nested
        .borrow_mut()
        .add_dictionary_handlers(|_, _| {}, |_| {});
    nested
        .borrow_mut()
        .add_dictionary_key_handler(b"line\\n", leaf.clone());
    nested
        .borrow_mut()
        .add_dictionary_key_handler(b"plain", leaf);

    let mut root = JsonHandler::new();
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
    let mut handler = JsonHandler::new();

    let error = handler
        .handle(b".", Json::make_string(b"oops"))
        .unwrap_err();

    assert_eq!(
        error.to_string(),
        "JSON handler: value at . is not of expected type"
    );
    assert_eq!(
        error,
        flpdf::json::JsonHandlerError("JSON handler: value at . is not of expected type".into())
    );
}

#[test]
fn unconfigured_type_handlers_reject_null_and_containers() {
    let mut handler = JsonHandler::new();
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
}

#[test]
fn unexpected_nested_key_reports_its_object_path_and_skips_end_handlers() {
    let ends = Rc::new(RefCell::new(Vec::new()));
    let nested = JsonHandler::shared();
    nested.borrow_mut().add_dictionary_handlers(|_, _| {}, {
        let ends = ends.clone();
        move |path| {
            ends.borrow_mut()
                .push(String::from_utf8_lossy(path).into_owned());
        }
    });

    let mut root = JsonHandler::new();
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
    let handler = JsonHandler::shared();
    handler.borrow_mut().add_dictionary_handlers(
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
    handler.borrow_mut().add_string_handler({
        let seen = seen.clone();
        move |path, value| {
            seen.borrow_mut().push(format!(
                "{}={}",
                String::from_utf8_lossy(path),
                String::from_utf8_lossy(value)
            ));
        }
    });
    handler
        .borrow_mut()
        .add_fallback_dictionary_handler(handler.clone());

    handler
        .borrow_mut()
        .handle(b".", Json::parse(br#"{"a":{"b":"done"}}"#).unwrap())
        .unwrap();

    assert_eq!(
        &*seen.borrow(),
        &["start:.", "start:.a", ".a.b=done", "end:.a", "end:.",]
    );
}

#[test]
fn mutually_recursive_dictionary_fallbacks_handle_a_finite_cycle() {
    let seen = Rc::new(RefCell::new(Vec::new()));
    let first = JsonHandler::shared();
    let second = JsonHandler::shared();
    first.borrow_mut().add_dictionary_handlers(
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
    first.borrow_mut().add_string_handler({
        let seen = seen.clone();
        move |path, value| {
            seen.borrow_mut().push(format!(
                "{}={}",
                String::from_utf8_lossy(path),
                String::from_utf8_lossy(value)
            ));
        }
    });
    second.borrow_mut().add_dictionary_handlers(
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
    first
        .borrow_mut()
        .add_fallback_dictionary_handler(second.clone());
    second
        .borrow_mut()
        .add_fallback_dictionary_handler(first.clone());

    first
        .borrow_mut()
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
fn breaking_registration_cycle_leaves_remaining_edge_strong() {
    let seen = Rc::new(RefCell::new(Vec::new()));
    let first = JsonHandler::shared();
    let second = JsonHandler::shared();
    let terminal = JsonHandler::shared();
    terminal.borrow_mut().add_number_handler({
        let seen = seen.clone();
        move |path, number| {
            seen.borrow_mut().push(format!(
                "{}={}",
                String::from_utf8_lossy(path),
                String::from_utf8_lossy(number)
            ));
        }
    });
    first.borrow_mut().add_fallback_handler(second.clone());
    second.borrow_mut().add_fallback_handler(first.clone());
    first.borrow_mut().add_fallback_handler(terminal.clone());

    let first_weak = Rc::downgrade(&first);
    drop(first);
    JsonHandler::handle_shared(&second, b".value", Json::make_int(1)).unwrap();
    assert_eq!(&*seen.borrow(), &[".value=1"]);
    assert!(first_weak.upgrade().is_some());

    second.borrow_mut().add_fallback_handler(terminal);
    assert!(first_weak.upgrade().is_none());
}

#[test]
fn handler_configuration_after_registration_is_used_at_dispatch_time() {
    let seen = Rc::new(RefCell::new(Vec::new()));
    let child = JsonHandler::shared();
    let mut root = JsonHandler::new();
    root.add_dictionary_handlers(|_, _| {}, |_| {});
    root.add_dictionary_key_handler(b"late", child.clone());

    child.borrow_mut().add_string_handler({
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
fn shared_entry_point_allows_callback_reentry_into_a_different_nested_callback() {
    let seen = Rc::new(RefCell::new(Vec::new()));
    let handler = JsonHandler::shared();
    let weak = Rc::downgrade(&handler);
    handler.borrow_mut().add_null_handler({
        let seen = seen.clone();
        move |path| {
            seen.borrow_mut()
                .push(format!("null:{}", String::from_utf8_lossy(path)));
        }
    });
    handler.borrow_mut().add_string_handler({
        let handler = Rc::downgrade(&handler);
        let seen = seen.clone();
        move |path, value| {
            seen.borrow_mut().push(format!(
                "string:{}={}",
                String::from_utf8_lossy(path),
                String::from_utf8_lossy(value)
            ));
            JsonHandler::handle_shared(
                &handler
                    .upgrade()
                    .expect("handler must be alive during callback reentry"),
                b".nested",
                Json::make_null(),
            )
            .unwrap();
        }
    });

    JsonHandler::handle_shared(&handler, b".", Json::make_string(b"outer")).unwrap();

    assert_eq!(&*seen.borrow(), &["string:.=outer", "null:.nested"]);

    drop(handler);
    assert!(weak.upgrade().is_none());
}
