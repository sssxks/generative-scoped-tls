# generative-scoped-tls

Scoped TLS with **bare references** and *native tls lookup*.

Yes this is possible. usage:

```rust
use generative_scoped_tls::{scoped, scoped_thread_local};

struct Context {
    answer: u32,
}

scoped_thread_local!(static CX: Context);

fn deep() {
    scoped!(let cx = CX);
    cx.answer; // `cx: &Context`.
}

fn main() {
    let cx = Context { answer: 42 };
    let body = || deep();

    // SAFETY: the call tree is synchronous.
    unsafe { CX.set(&cx, body) };
}
```

To use scoped tls, assert no suspension using the outmost `unsafe` block.

Reminder: place the callback before entering the `unsafe` block to prevent accidental unsafe throughout body.

## The Idea

Scoped TLS stores a pointer to outer stack frame in a special TLS register, allowing context passed implicitly through call stack.

```text
caller stack                     native TLS
┌──────────────┐               ┌──────────────┐
│      T       │ ◀──────────── │  *const ()   │
└──────────────┘               └──────────────┘

lifetime(&T from scoped!)
    ⊆ scope(deep)
    ⊆ scope(ScopedKey::set)
    ⊆ lifetime(T)
```

- `generativity` crate solves `lifetime(&T from scoped!) ⊆ scope(deep)`.
- manual synchronous assertion ensures `scope(deep) ⊆ scope(ScopedKey::set)`.
- `scope(ScopedKey::set) ⊆ lifetime(T)` is enforced by normal borrowck.

A suspended future can break `scope(deep) ⊆ scope(ScopedKey::set)` by retaining `&T`, returning `Pending` so that `set` returns, and later resuming.

However, for synchronous call tree, the intended discipline is natural; and for async usages, tls will likely cause semantic issues, and we expect the usage for task_local storage instead.

## Performance

`scoped!` incurs very little overhead:

```text
native TLS pointer load
→ null check
→ raw pointer becomes &T
```

After binding the resulting `&T`, repeated access is ordinary reference access.

## Status

Prototype. Use at your own risk.
