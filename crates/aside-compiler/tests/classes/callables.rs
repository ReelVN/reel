//! Function boundaries retain identity and use the ordinary sliced call machinery.
use super::*;

#[test]
fn callable_contracts_support_closures_methods_defaults_rest_and_builtins() {
    for source in [
        "fn make(seed:int)->fn(int)->int{|n|seed+n}fn main(){let add=make(40);add(2)}",
        "fn apply(f:fn(int)->int,value:int)->int{f(value)}fn add(n:int,extra:int=2)->int{n+extra}fn main(){apply(add,40)}",
        "fn total(...items){items[0]+items[1]}fn main(){let f:fn(int,int)->int=total;f(...[40,2])}",
        "class Counter{let mut n=40;fn add(self,n:int)->int{self.n+=n;self.n}}fn main(){let c=Counter();let f:fn(int)->int=c.add;let alias=f; if f!=alias{throw 0;}f(2)}",
        "fn main(){let items=[1,2];let size:fn()->int=items.len;let append:fn(int)->any=items.append;append(3);if size()==3{42}else{0}}",
        "class Scene{let n=42;}fn show(scene:Scene)->Scene{scene}fn main(){let f:fn(Scene)->Scene=show;f(Scene()).n}",
        "fn main(){let absent:(fn(int)->int)|null=null;if absent?(2)!=null{throw 0;}let f:(fn(int)->int)|null=|n|n+2;f?(40)}",
        "fn main(){let captured={value:40};let f:fn(int)->int=|n|{captured.value+=n;captured.value};let old=f;let callbacks=[f];if old!=callbacks[0]{throw 0;}f(2)}",
        "fn main(){let f:fn((int,string))->(int,string)=|value|value;let pair=f((42,\"ok\"));pair[0]}",
    ] {
        run(source);
    }
}

#[test]
fn callable_contracts_reject_dynamic_values_before_body_and_check_results_at_call() {
    run(r#"fn dynamic(value){value}
        fn main(){
            let mut entered=0;
            let f:fn(int)->int=|value|{entered+=1;value};
            let mut rejected=0;
            try{f(dynamic("wrong"));}catch error{rejected+=1;}
            try{f(...dynamic([]));}catch error{rejected+=1;}
            try{f(...dynamic([1,2]));}catch error{rejected+=1;}
            try{f(...dynamic({value:1}));}catch error{rejected+=1;}
            if entered!=0{throw "body entered with invalid arguments";}
            let wrong:fn()->int=dynamic(||"wrong");
            try{wrong();}catch error{rejected+=1;}
            try{let bad:fn()->int=dynamic(1);}catch error{rejected+=1;}
            let list=[];
            let append:fn(int)->any=list.append;
            try{append(dynamic("wrong"));}catch error{rejected+=1;}
            if rejected!=7 || list.len()!=0{throw "contract";}
            f(42)
        }"#);
}
