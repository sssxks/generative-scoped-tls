# generative-scoped-tls

Experimental **synchronous scoped TLS** with native pointer-speed lookup and ergonomic bare references.

The design is:

```text
scoped TLS
+ generativity
+ bare &T
```

The physical representation follows the classic `scoped-tls` idea:

```text
caller stack                     native TLS
┌──────────────┐               ┌──────────────┐
│      T       │ ◀──────────── │  *const ()   │
└──────────────┘               └──────────────┘
```

`set` temporarily installs `&T` as a raw pointer in a `const`-initialized native TLS slot. `scoped_get!` creates a fresh `generativity` brand at the lookup site and reborrows that pointer as a normal `&T` whose lifetime is bounded by the caller's lexical scope.

## API

```rust
use generative_scoped_tls::{scoped_get, scoped_thread_local};

struct Context {
    answer: u32,
}

scoped_thread_local!(static CX: Context);

fn deep() {
    scoped_get!(let cx = CX);

    // `cx` really is `&Context`.
    cx.answer;
}

fn compile() {
    let cx = Context { answer: 42 };

    // SAFETY: the call tree is synchronous. No generative TLS reference can
    // remain live after this dynamic `set` scope returns.
    unsafe {
        CX.set(&cx, || {
            deep();
        });
    }
}
```

The one `unsafe` is deliberately placed at the outer scope boundary. Deep callees get a plain reference without a token, wrapper, `Rc`, allocation, or CPS callback.

## Why `scoped_get!` is a statement macro

We would like to write:

```rust,ignore
let cx = CX.get();
```

but a zero-argument Rust function has no lifetime input from which to derive the caller-bounded result lifetime.

`generativity::make_guard!` solves that by creating a fresh invariant lifetime *at the lookup site*. Its hidden lifetime carrier must remain in the surrounding block, so the ergonomic form is:

```rust
# use generative_scoped_tls::{scoped_get, scoped_thread_local};
# scoped_thread_local!(static CX: u32);
# let value = 1;
# unsafe { CX.set(&value, || {
scoped_get!(let cx = CX);
# let _: &u32 = cx;
# }); }
```

not an expression macro whose hidden carrier would immediately go out of scope.

## The async / suspension boundary

This crate deliberately models a **synchronous call-stack scope**.

`generativity` proves a lexical lifetime, while the raw pointer is valid for a dynamic `set` extent. Ordinary synchronous calls satisfy:

```text
lifetime(&T from scoped_get!)
    ⊆ lexical callee scope
    ⊆ dynamic set scope
    ⊆ lifetime(original T)
```

A suspended future/coroutine can break the middle inclusion by retaining `&T`, returning `Pending` so that `set` returns, and later resuming. Therefore `ScopedKey::set` is `unsafe` and its safety contract explicitly excludes that behavior.

This is not merely a task-local-vs-thread-local semantic issue: allowing the reference to survive after the stack-owned `T` dies would be memory unsafety.

For a compiler or other synchronous recursive call tree, the intended discipline is natural and the unsafe boundary is normally written once around the top-level operation.

## Performance model

Hot `scoped_get!` is conceptually:

```text
native TLS pointer load
→ null check
→ raw pointer becomes &T
```

After binding the resulting `&T`, repeated field/method access is ordinary reference access with no TLS revalidation.

There is:

- no heap allocation;
- no `Rc` / `Arc`;
- no atomic operation;
- no concurrent map/thread-ID lookup;
- no per-dereference wrapper check;
- no TLS destructor for the slot (`Cell<*const ()>` is non-dropping).

The exact machine instructions for native TLS access remain target/link-model dependent.

## Unwinding and nesting

`set` uses an RAII reset object. Normal return and panic unwinding restore the previous TLS pointer. Nested `set` calls are supported:

```text
set(A)
  TLS → A
  set(B)
    TLS → B
  return
  TLS → A
return
TLS → previous/null
```

The actual values remain owned by their normal stack frames and are dropped normally during unwind.

## Safety boundary

The unsafe contract is intentionally concentrated at `set`:

```rust,ignore
unsafe { CX.set(&cx, || whole_sync_call_tree()) }
```

Safe deep code may then use:

```rust,ignore
scoped_get!(let cx = CX); // cx: &Context
```

Do **not** use a borrowed result across an async `.await`, generator/coroutine yield, or manual future polling pattern that allows the corresponding `set` call to end while the borrow remains live.

## Status

Research prototype. The unsafe core is small and documented, but this environment did not have `rustc`/`cargo` installed, so the crate has not been compiled or run here. Before production use, run at minimum:

```text
cargo test
cargo test --doc
cargo miri test
```

and add compile-fail coverage for lexical escape attempts.
