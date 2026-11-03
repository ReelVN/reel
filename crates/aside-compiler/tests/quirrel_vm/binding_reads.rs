//! Frame-local borrowing keeps evaluation snapshots, shared cells and cleanup.
use super::*;

#[test]
fn frame_local_borrows_preserve_source_writes_and_cross_call_values() {
    let source = r#"
        fn touch(log) { log.append(7); 3 }
        fn sample(flag) {
            let log=[];
            let mut time=2;
            if flag { time=4; }
            let sum=time+touch(log);
            assert(sum==(if flag {7} else {5}));
            assert(log[0]==7);
            let saved=time+(if true {time=9;1} else {0});
            assert(saved==(if flag {5} else {3}));
            assert(time==9);
            let mut left=0;
            let mut right=5;
            while right-left>1 {
                let middle=(left+right)>>1;
                if middle<3 {left=middle;} else {right=middle;}
            }
            assert(left==2);
            assert(right==3);
        }
        sample(false);
        sample(true);
    "#;
    for tiny in [false, true] {
        assert_eq!(execute(source, tiny).unwrap(), (String::new(), 12));
    }
}

#[test]
fn shared_cell_reads_keep_the_value_observed_before_a_callback() {
    let source = r#"
        fn sample() {
            let mut value=2;
            fn update() {value=9;3}
            let saved=value+update();
            assert(saved==5);
            assert(value==9);
        }
        sample();
    "#;
    for tiny in [false, true] {
        assert_eq!(execute(source, tiny).unwrap(), (String::new(), 2));
    }
}

#[test]
fn borrowed_values_keep_exception_and_finally_order_across_loop_edges() {
    let source = r#"
        fn sample(flag) {
            let mut value=1;
            try {
                value=2;
                if flag {throw 3;}
                assert(value==2);
            } catch caught {
                assert(caught==3);
                assert(value==2);
            } finally {value=value+4;}
            assert(value==6);
        }
        sample(false);
        sample(true);
        fn looped() {
            let mut index=0;
            let mut total=0;
            while index<4 {
                try {
                    index=index+1;
                    if index==2 {continue;}
                    total=total+index;
                } finally {total=total+10;}
            }
            total
        }
        assert(looped()==48);
    "#;
    for tiny in [false, true] {
        assert_eq!(execute(source, tiny).unwrap(), (String::new(), 6));
    }
}
