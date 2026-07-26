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
        .handle(b".", Json::parse(br#"{"known":{"x":"y"}}"#).unwrap())
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
    handler.borrow_mut().add_null_handler({
        let seen = seen.clone();
        move |path| {
            seen.borrow_mut()
                .push(format!("null:{}", String::from_utf8_lossy(path)));
        }
    });
    handler.borrow_mut().add_string_handler({
        let handler = handler.clone();
        let seen = seen.clone();
        move |path, value| {
            seen.borrow_mut().push(format!(
                "string:{}={}",
                String::from_utf8_lossy(path),
                String::from_utf8_lossy(value)
            ));
            JsonHandler::handle_shared(&handler, b".nested", Json::make_null()).unwrap();
        }
    });

    JsonHandler::handle_shared(&handler, b".", Json::make_string(b"outer")).unwrap();

    assert_eq!(&*seen.borrow(), &["string:.=outer", "null:.nested"]);
}
