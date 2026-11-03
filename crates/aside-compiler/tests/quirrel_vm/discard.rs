use super::*;

#[test]
fn discarded_control_results_preserve_lazy_evaluation_order_and_value_results() {
    let source = r#"
        fn mark(log, value) { log.append(value); value }
        let log=[];
        mark(log,1)==0 && mark(log,99)==0;
        mark(log,2)==2 || mark(log,99)==0;
        mark(log,3) ?? mark(log,99);
        null ?? mark(log,4);
        if mark(log,5)==5 && mark(log,6)==6 { mark(log,7); } else { mark(log,99); }
        let chosen=if (mark(log,8)==8 || mark(log,99)==0) && mark(log,9)==9 { mark(log,10) } else { 99 };
        assert(chosen==10);
        while mark(log,12)==12 && false { mark(log,99); }
        assert(log.len()==11);
        for index in 0..10 { assert(log[index]==index+1); }
        assert(log[10]==12);
    "#;
    for tiny in [false, true] {
        assert_eq!(execute(source, tiny).unwrap(), (String::new(), 13));
    }
}

#[test]
fn discard_retains_dynamic_errors_and_checks_only_evaluated_logical_operands() {
    for (expression, argument, fault) in [
        ("value && true", "1", "Type"),
        ("true && value", "1", "Type"),
        ("value || false", "1", "Type"),
        ("false || value", "1", "Type"),
        ("1 / value", "0", "DivisionByZero"),
        ("value.absent", "({})", "MissingKey"),
    ] {
        let source = format!("fn fail(value) {{ {expression}; }} fail({argument});");
        for tiny in [false, true] {
            let error = execute(&source, tiny).unwrap_err();
            assert!(error.contains(fault), "{expression}: {error}");
        }
    }
    for tiny in [false, true] {
        assert_eq!(
            execute(
                "fn skip(value){ false && value; true || value; } skip(1);",
                tiny
            )
            .unwrap(),
            (String::new(), 0)
        );
    }
}

#[test]
fn discarded_blocks_keep_children_and_finally_and_terminal_short_circuit_edges() {
    let source = r#"
        fn gather(children=[]) { children }
        fn mark(log, value) { log.append(value); }
        let log=[];
        let children=gather(){
            mark(log,1);
            if true { mark(log,2); } else { mark(log,99); }
            for value in 3..5 { mark(log,value); }
            try { mark(log,5); } finally { mark(log,6); }
        };
        assert(children.len()==0);
        for index in 0..6 { assert(log[index]==index+1); }
        fn terminal(flag) { if (flag && (if flag { return 7; } else { true })) || (if flag { false } else { return 9; }) { return 1; } 2 }
        assert(terminal(true)==7);
        assert(terminal(false)==9);
        fn done(log) { try { if true { return 3; } } finally { mark(log,7); } }
        assert(done(log)==3);
        assert(log[6]==7);
    "#;
    for tiny in [false, true] {
        assert_eq!(execute(source, tiny).unwrap(), (String::new(), 11));
    }
}

#[test]
fn children_if_still_consumes_and_validates_each_produced_value() {
    let source = "fn gather(children=[]) { children } fn mark(log,value){log.append(value);value} \
                  let log=[]; gather(){if true {mark(log,1);}};";
    for tiny in [false, true] {
        assert!(execute(source, tiny).unwrap_err().contains("Type"));
    }
}

#[test]
fn terminal_logical_operands_keep_return_throw_and_outer_short_circuit_edges() {
    let source = r#"
        fn and_or(a,b) {
            if (a && ((if b {return 1;} else {throw 2;}) == null)) || ((if b {return 3;} else {throw 4;}) == null) {return 5;}
            6
        }
        fn or_and(a,b) {
            if (a || ((if b {return 1;} else {throw 2;}) == null)) && ((if b {return 3;} else {throw 4;}) == null) {return 5;}
            6
        }
        assert(and_or(true,true)==1);
        assert(and_or(false,true)==3);
        assert(or_and(false,true)==1);
        assert(or_and(true,true)==3);
        try {and_or(true,false);} catch caught {print(caught);}
        try {and_or(false,false);} catch caught {print(caught);}
        try {or_and(false,false);} catch caught {print(caught);}
        try {or_and(true,false);} catch caught {print(caught);}
    "#;
    for tiny in [false, true] {
        assert_eq!(execute(source, tiny).unwrap(), ("2424".into(), 4));
    }
}

#[test]
fn one_sided_terminal_branches_and_loops_preserve_the_remaining_exit() {
    let source = r#"
        fn only_no(flag){ if flag && ((if flag{return 9;}else{throw 8;})==null){return 3;}7 }
        fn only_yes(flag){ if flag || ((if flag{return 9;}else{throw 8;})==null){return 3;}7 }
        fn loop_no(flag){ while flag && ((if flag{return 9;}else{throw 8;})==null){let value=4;}7 }
        assert(only_no(false)==7);
        assert(only_no(true)==9);
        assert(only_yes(true)==3);
        try {only_yes(false);} catch caught {assert(caught==8);}
        assert(loop_no(false)==7);
        assert(loop_no(true)==9);
    "#;
    for tiny in [false, true] {
        assert_eq!(execute(source, tiny).unwrap(), (String::new(), 6));
    }
}
