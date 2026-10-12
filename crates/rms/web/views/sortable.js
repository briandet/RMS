// Reordering a list by dragging its rows' handles (pointer events: mouse, pen and touch alike,
// unlike HTML5 drag and drop), or with the arrow keys on a focused handle.  Used by the queue
// and by playlists.

import { scrollParent } from '../util.js';

/**
 * Make `list`'s <li> children reorderable by their `.drag-handle`.  `onStart()` is called when
 * a drag begins; `onDrop(from, to)` when it ends (`to === from`: nothing moved, or cancelled).
 * After a keyboard move the handle at the new place gets the focus.
 */
export function sortable(list, { onStart, onDrop }) {
  list.addEventListener('keydown', (e) => {
    const handle = e.target.closest('.drag-handle');
    if (!handle || (e.key !== 'ArrowUp' && e.key !== 'ArrowDown')) return;
    e.preventDefault();
    const items = rows(list);
    const from = items.indexOf(handle.closest('li'));
    const to = from + (e.key === 'ArrowUp' ? -1 : 1);
    if (from < 0 || to < 0 || to >= items.length) return;
    onDrop(from, to);
    // (moving or redrawing the row loses the focus: give it back at once, and again once drawn)
    const refocus = () => rows(list)[to]?.querySelector('.drag-handle')?.focus();
    refocus();
    requestAnimationFrame(refocus);
  });

  list.addEventListener('pointerdown', (e) => {
    const handle = e.target.closest('.drag-handle');
    if (!handle || e.button !== 0) return;
    e.preventDefault();
    const li = handle.closest('li');
    const items = rows(list);
    const from = items.indexOf(li);
    const scroller = scrollParent(list);
    const scrollerRect = () => (scroller === document.scrollingElement || scroller === document.documentElement
      ? { top: 0, bottom: innerHeight } : scroller.getBoundingClientRect());
    const startY = e.clientY, startScroll = scroller.scrollTop;
    let y = startY, slot = from, raf = 0;
    onStart?.();
    try { handle.setPointerCapture(e.pointerId); } catch { /* ignore */ }
    li.classList.add('dragging');
    list.classList.add('is-dragging');

    const update = () => {
      li.style.transform = `translateY(${y - startY + (scroller.scrollTop - startScroll)}px)`;
      slot = items.length;
      for (let i = 0; i < items.length; i++) {
        if (items[i] === li) continue;
        const r = items[i].getBoundingClientRect();
        if (y < r.top + r.height / 2) { slot = i; break; }
      }
      for (const x of items) x.classList.remove('drop-before', 'drop-after');
      const noop = slot === from || slot === from + 1;
      if (!noop) {
        if (slot < items.length) items[slot].classList.add('drop-before');
        else items[items.length - 1].classList.add('drop-after');
      }
    };
    const autoScroll = () => {
      const r = scrollerRect();
      const edge = 56;
      let v = 0;
      if (y < r.top + edge) v = -Math.ceil((r.top + edge - y) / 4);
      else if (y > r.bottom - edge) v = Math.ceil((y - (r.bottom - edge)) / 4);
      if (v) { scroller.scrollTop += v; update(); }
      raf = requestAnimationFrame(autoScroll);
    };
    const onMove = (ev) => { y = ev.clientY; update(); };
    const onUp = (ev) => {
      cancelAnimationFrame(raf);
      handle.removeEventListener('pointermove', onMove);
      handle.removeEventListener('pointerup', onUp);
      handle.removeEventListener('pointercancel', onUp);
      li.style.transform = '';
      li.classList.remove('dragging');
      list.classList.remove('is-dragging');
      for (const x of items) x.classList.remove('drop-before', 'drop-after');
      if (ev.type === 'pointercancel') { onDrop(from, from); return; }
      onDrop(from, slot > from ? slot - 1 : slot);
    };
    handle.addEventListener('pointermove', onMove);
    handle.addEventListener('pointerup', onUp);
    handle.addEventListener('pointercancel', onUp);
    raf = requestAnimationFrame(autoScroll);
  });
}

function rows(list) {
  return [...list.children].filter((c) => c.tagName === 'LI' && !c.classList.contains('letter'));
}

/** `order` (0…n-1) with the entry at `from` moved to `to`. */
export function moved(n, from, to) {
  const order = Array.from({ length: n }, (_, i) => i);
  const [x] = order.splice(from, 1);
  order.splice(to, 0, x);
  return order;
}
