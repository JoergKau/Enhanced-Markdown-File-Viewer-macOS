# MD-Viewer

Welcome! This is **start-up.md**, the document shown when MD-Viewer launches.

- Edit this file to make the welcome screen your own.
- Delete it and MD-Viewer starts with an empty document — no error.
- It is opened as an untitled document; use **Save As…** to keep changes.

Press **Edit** to change the Markdown; this is the rendered preview —
via [wgpu](https://wgpu.rs) on Metal.

## Supported formats

Text styles: **bold**, *italic*, ~~strikethrough~~, `inline_code` and a [link](https://egui.rs).

Task list:

- [x] Parsing via pulldown-cmark
- [x] wgpu rendering on Metal
- [ ] Write your own notes

1. Ordered item
2. Another one
   - nested

> Block quote over two lines
>
> > Nested!

> [!NOTE]
> GFM alerts: NOTE / TIP / IMPORTANT / WARNING / CAUTION.

---

### Table (GFM)

| Feature  | Status | Note              |
| :------- | :----: | ----------------: |
| Tables   |   ✓    | multi-column      |
| Math     |   ✓    | typeset with LaTeX |

### Code block

```rust
fn main() {
    let x = 42; // comment
    let msg = String::from("Hello world");
    println!("{msg}: {x}");
}
```

### Math & footnotes

Inline: $E = mc^2$; display:

$$\int_0^\infty e^{-x}\,dx = 1$$

See footnote[^1].

[^1]: A footnote — typeset cleanly at the end of the PDF.
