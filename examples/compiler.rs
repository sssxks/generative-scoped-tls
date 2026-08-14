use generative_scoped_tls::{scoped, scoped_thread_local};
use std::cell::RefCell;

#[derive(Default)]
struct Context {
    diagnostics: RefCell<Vec<String>>,
}

scoped_thread_local!(static CX: Context);

fn parse() {
    scoped!(let cx = CX);
    cx.diagnostics.borrow_mut().push("parsed".into());
}

fn typecheck() {
    scoped!(let cx = CX);
    cx.diagnostics.borrow_mut().push("checked".into());
}

fn compile() {
    let cx = Context::default();

    let body = || {
        parse();
        typecheck();
    };

    // SAFETY: parse/typecheck and everything they call are synchronous. No
    // reference obtained from CX is retained after this dynamic scope returns.
    unsafe { CX.set(&cx, body) };

    assert_eq!(cx.diagnostics.into_inner(), ["parsed", "checked"]);
}

fn main() {
    compile();
}
