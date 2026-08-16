# generative-scoped-tls

Scoped TLS with **bare references** and native TLS lookup.

```rust
use generative_scoped_tls::{scoped, scoped_thread_local};

struct Context {
    answer: u32,
}

scoped_thread_local!(static CX: Context);

fn deep() {
    let cx = scoped!(CX);
    assert_eq!(cx.answer, 42);
}

fn main() {
    let cx = Context { answer: 42 };
    let body = || deep();

    // SAFETY: the call tree is synchronous.
    unsafe { CX.set(&cx, body) }
}
```

To use scoped TLS, manually assert no suspension using the outermost `unsafe`
`set` call. Construct the callback before entering that unsafe block so deep code
remains a normal safe context.

## The idea

Scoped TLS stores a pointer to an outer stack frame in a native TLS slot:

```text
caller stack                     native TLS
┌──────────────┐               ┌──────────────┐
│      T       │ ◀──────────── │  *const ()   │
└──────────────┘               └──────────────┘
```

`scoped!(CX)` creates a temporary `ScopedRef<T>` containing the current raw
pointer and returns `&*that_temporary`. `ScopedRef<T>: Deref<Target = T>`, so the
result is an ordinary borrow tied to the proxy:

```text
lifetime(&T)
    ⊆ lifetime(ScopedRef temporary)
    ⊆ lexical caller scope
    ⊆ dynamic scope(ScopedKey::set)
    ⊆ lifetime(installed T)
```

The first two inclusions come from normal borrow checking plus Rust temporary
lifetime extension for a `let x = &*temporary` initializer. The third inclusion
is the caller's unsafe synchronous-execution assertion. The last follows from
the borrow passed to `set`.

No fresh invariant lifetime brand is required: this crate needs only an upper
bound on the reconstructed reference lifetime, not a globally unique lifetime
identity.

A suspended future can break `lexical caller scope ⊆ dynamic scope(set)` by
retaining the proxy/reference, returning `Pending` so `set` returns, and later
resuming. Use task-local storage for async control flow instead.

## Performance

A lookup performs one native TLS access and one null check. After
`let cx = scoped!(CX);`, subsequent accesses are ordinary reference accesses.

The null check exists because Rust has no lightweight syntax for declaring these
implicit dynamic dependencies. If you need to eliminate it, explicit argument
passing remains the simplest option.

## Status

Experimental prototype. Use at your own risk.
