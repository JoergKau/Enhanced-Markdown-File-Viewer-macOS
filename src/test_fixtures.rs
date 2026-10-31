//! Self-contained Markdown fixtures for the test suite.
//!
//! Everything here is compiled into the test binary, so `cargo test` works in
//! a minimal checkout that ships only the application sources. No test reads a
//! `.md` file from the repository (only `README.md`, which is part of the
//! published tree).
//!
//! * [`FEATURE_GALLERY`] contains every block and inline construct the parser
//!   can produce and is used by the parser coverage test and by the headless
//!   preview render test in both themes.
//! * [`COMPATIBILITY_SAMPLE`] concentrates the extended constructs (front
//!   matter, callouts, HTML conversion, footnotes, math, Mermaid, definition
//!   lists, wiki links) whose behaviour is asserted in detail by
//!   `parser::extended_tests`.
//!
//! When a new `Block` or `Inline` variant is added, extend [`FEATURE_GALLERY`]
//! so the coverage test keeps passing (the exhaustive `block_kind` /
//! `inline_kind` matches already fail compilation until the variant is listed).

/// Every supported construct, used by the coverage and headless render tests.
pub const FEATURE_GALLERY: &str = r#"
---
title: Gallery
tags: [one, two]
---

# Heading 1
## Heading 2
### Heading 3
#### Heading 4
##### Heading 5
###### Heading 6

Text with **bold**, *italic*, ~~strike~~, ==highlight==, <u>underline</u>,
<kbd>Ctrl</kbd>, `code`, $E=mc^2$, a [link](https://example.org),
$H~2~O$, x^2^, [[Wiki Link]], ![[Embedded Note]] and a footnote.[^n]

![Missing image](does-not-exist.png)

[^n]: Footnote body with **markup**.

- Unordered item
  - Nested item
- [x] Done task
- [ ] Open task

1. Ordered
2. Second

> A quote.
>
> > Nested quote.

> [!NOTE]
> A callout with **bold** text.

```rust
fn main() {
    println!("code");
}
```

```mermaid
graph TD
    A[Start] --> B[End]
```

| Left | Center | Right |
| :--- | :----: | ----: |
| a | b | c |

$$
a^2 + b^2 = c^2
$$

Term
: Definition body.

<details open>
<summary>Details summary</summary>

Body inside details.

</details>

<div style="background:#eee; border-left: 4px solid #2196f3;">
<strong>HTML banner</strong> with <em>inline markup</em>.
</div>

<section>
<h2>HTML section</h2>
<p>Paragraph with <mark>mark</mark>.</p>
<ul><li>HTML list item</li></ul>
</section>

<table>
<thead><tr><th>HTML</th><th>Table</th></tr></thead>
<tbody><tr><td>1</td><td>2</td></tr></tbody>
</table>

<p align="center"><img src="missing-icon.png" width="80" alt="Icon"></p>

<source srcset="x">

::: note
A Pandoc fenced div with **markup**.
:::

!!! tip "MkDocs admonition"
    Body of the admonition.

*[HTML]: HyperText Markup Language

A #project tag, a citation [@doe2024], and HTML text.

[TOC]

<video src="clip.mp4">
</video>

---

Done.
"#;

/// Extended constructs with values asserted by `parser::extended_tests`.
pub const COMPATIBILITY_SAMPLE: &str = r#"
---
title: Compatibility Sample
extensions:
  - Mermaid
  - Tables
meta:
  author: Ada
---

# Compatibility Sample

> [!WARNING] Custom callout title
> Warning body.

> [!tip]-
> Collapsed tip body.

> [!QUESTION]+
> Expanded question body.

A ==Common highlight extension==, H~2~O and x^2^.

![Remote placeholder image](https://example.com/remote.png "Remote image title")

![[Pasted image 20240101.png]]

<img src="local.png" alt="HTML image test" width="320" height="168">

Press <kbd>Ctrl</kbd> and see <mark>HTML mark element</mark>.

<details>
<summary>Expandable details test</summary>

Hidden body.

</details>

<table>
<thead><tr><th>HTML</th><th>Table</th></tr></thead>
<tbody><tr><td>1</td><td>2</td></tr></tbody>
</table>

<script>alert('never runs')</script>

<!-- hidden html comment -->

| URL |
| --- |
| https://example.com/a/very/long/path/that/tests/wrapping/behaviour |

Reference one[^standard-note] and two[^long-note].

[^standard-note]: First note.
[^long-note]: Second note.

$$
\begin{bmatrix}
a & b \\
c & d
\end{bmatrix}
$$

$$
\mathbf{A}
$$

```mermaid
graph TD
    A --> B
```

```mermaid
sequenceDiagram
    A->>B: Hello
```

```mermaid
pie
    "A" : 1
```

```mermaid
classDiagram
    A <|-- B
```

```mermaid
stateDiagram-v2
    [*] --> A
```

```mermaid
gantt
    title G
    section S
    Task :a1, 2024-01-01, 1d
```

```mermaid
erDiagram
    A ||--o{ B : has
```

Term one
: Definition one.

Term two
: Definition two.

Term three
: Definition three.

See [[Project Alpha]] and ![[Embedded Note]].
"#;
