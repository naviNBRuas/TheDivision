# Copy-to-Clipboard Button

Copies text on click; the copy icon morphs into a checkmark for ~1.5 s, then
reverts. Uses the async Clipboard API with an `execCommand` fallback for old
browsers, and reports the result to screen readers via `aria-live`.

## When to use

- Code blocks, install commands, API keys, share links — anywhere a user would otherwise select text by hand.

## Markup

```html
<div class="copyable">
  <pre><code id="cmd">npm install @nbr/divisi</code></pre>
  <button class="copy-btn" data-copy="#cmd" aria-label="Copy to clipboard">
    <svg class="icon icon--copy"></svg>
    <svg class="icon icon--check" aria-hidden="true"></svg>
  </button>
  <span class="visually-hidden" role="status" aria-live="polite"></span>
</div>
```

## Behavior

```js
const RESET_MS = 1500;

async function writeClipboard(text) {
  if (navigator.clipboard?.writeText) return navigator.clipboard.writeText(text);
  // Fallback: older browsers and non-secure origins.
  const ta = Object.assign(document.createElement("textarea"), { value: text });
  ta.setAttribute("readonly", "");
  ta.style.cssText = "position:fixed;opacity:0";
  document.body.append(ta); ta.select();
  const ok = document.execCommand("copy"); ta.remove();
  if (!ok) throw new Error("copy rejected");
}

function wireCopyButtons(root = document) {
  root.querySelectorAll(".copy-btn").forEach((btn) => {
    let timer;
    btn.addEventListener("click", async () => {
      const live = btn.closest(".copyable").querySelector("[role=status]");
      try {
        await writeClipboard(document.querySelector(btn.dataset.copy).textContent);
        btn.dataset.state = "copied";
        live.textContent = "Copied to clipboard";
      } catch {
        btn.dataset.state = "error";
        live.textContent = "Copy failed. Select the text and copy manually.";
      } finally {
        clearTimeout(timer);
        timer = setTimeout(() => delete btn.dataset.state, RESET_MS);
      }
    });
  });
}
```

## Icon morph (CSS)

```css
.icon--check { display: none; }
.copy-btn[data-state=copied] .icon--copy { display: none; }
.copy-btn[data-state=copied] .icon--check { display: block; animation: pop .2s ease-out; }
@media (prefers-reduced-motion: reduce) { .icon--check { animation: none; } }
```

## Accessibility

- Success must never be visual-only: the `role="status"` region announces it
  politely without moving focus; keep the region mounted so screen readers
  don't drop a freshly inserted announcement.

## Edge cases

- **Permission denied** (`NotAllowedError`): show the error state plus a
  manual-copy hint; never retry silently in a loop.
- Rapid re-clicks reset the timer in `finally`, so the checkmark never sticks.
