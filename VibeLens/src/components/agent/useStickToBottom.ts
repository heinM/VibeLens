import { useCallback, useEffect, useRef, useState } from "react";

/** Within this many pixels of the bottom, a scroll by the user re-engages following. */
export const REPIN_DISTANCE_PX = 120;
/** A scroll event this soon after wheel, touch or key input counts as the user's own. */
const INTENT_WINDOW_MS = 250;
const UP_KEYS = new Set(["ArrowUp", "PageUp", "Home"]);
const SCROLL_KEYS = new Set(["ArrowUp", "ArrowDown", "PageUp", "PageDown", "Home", "End", " "]);

/**
 * Whether to keep following after a scroll event. Only the user lets go: a scroll they made
 * upwards and away from the bottom unpins, one that ends near the bottom re-pins. Scrolls the
 * user did not make (content growing or shrinking, our own jump to the bottom) never change
 * anything, so a streaming answer cannot shake the view loose.
 */
export function pinAfterScroll(pinned: boolean, s: { byUser: boolean; movedUp: boolean; distanceFromBottom: number }): boolean {
  if (!s.byUser) return pinned;
  if (s.distanceFromBottom <= REPIN_DISTANCE_PX) return true;
  return s.movedUp ? false : pinned;
}

/**
 * Keeps a scroller pinned to the bottom while content grows. Only the user's own scrolling
 * (wheel, touch, keyboard, scrollbar drag) releases the pin; coming back near the bottom, or
 * `jumpToBottom` (Latest, sending a message), re-engages it.
 */
export function useStickToBottom(hasContent = true) {
  const scroller = useRef<HTMLDivElement>(null);
  const content = useRef<HTMLDivElement>(null);
  const pinned = useRef(true);
  const hasContentRef = useRef(hasContent);
  hasContentRef.current = hasContent;
  const [atBottom, setAtBottom] = useState(true);
  /** When the user last gave scrolling input, and whether a pointer is held on the scroller. */
  const intentAt = useRef(0);
  const pointerDown = useRef(false);
  const lastTop = useRef(0);
  const lastHeight = useRef(0);
  const touchY = useRef<number | null>(null);

  const setPinned = useCallback((v: boolean) => {
    pinned.current = v;
    setAtBottom(v);
  }, []);

  const toBottom = useCallback(() => {
    const el = scroller.current;
    if (el) {
      el.scrollTop = el.scrollHeight;
      lastTop.current = el.scrollTop;
      lastHeight.current = el.scrollHeight;
    }
  }, []);

  const onScroll = useCallback(() => {
    const el = scroller.current;
    if (!el) return;
    const top = el.scrollTop;
    const movedUp = top < lastTop.current;
    // Content never scrolls itself up without changing height (a shrink clamps scrollTop), so
    // an upward move at the same height is the user, whatever input it came from.
    const sameHeight = el.scrollHeight === lastHeight.current;
    const next = pinAfterScroll(pinned.current, {
      byUser: pointerDown.current || performance.now() - intentAt.current < INTENT_WINDOW_MS || (movedUp && sameHeight),
      movedUp,
      distanceFromBottom: el.scrollHeight - top - el.clientHeight,
    });
    lastTop.current = top;
    lastHeight.current = el.scrollHeight;
    if (next !== pinned.current) setPinned(next);
  }, [setPinned]);

  const jumpToBottom = useCallback(() => {
    setPinned(true);
    toBottom();
  }, [setPinned, toBottom]);

  // The user's scrolling input. Going up lets go at once, before the scroll event lands, so the
  // next growth of the content cannot pull the view back down under their hand.
  useEffect(() => {
    const el = scroller.current;
    if (!el) return;
    const mark = () => {
      intentAt.current = performance.now();
    };
    const release = () => {
      mark();
      if (pinned.current) setPinned(false);
    };
    const onWheel = (e: WheelEvent) => (e.deltaY < 0 ? release() : mark());
    const onTouchStart = (e: TouchEvent) => {
      touchY.current = e.touches[0]?.clientY ?? null;
      mark();
    };
    const onTouchMove = (e: TouchEvent) => {
      const y = e.touches[0]?.clientY;
      // A finger moving down scrolls the content up.
      if (y != null && touchY.current != null && y > touchY.current) release();
      else mark();
      touchY.current = y ?? null;
    };
    const onKeyDown = (e: KeyboardEvent) => {
      if (UP_KEYS.has(e.key) || (e.key === " " && e.shiftKey)) release();
      else if (SCROLL_KEYS.has(e.key)) mark();
    };
    // Dragging the scrollbar (or selecting text past the edge) scrolls while the pointer is held.
    const onPointerDown = () => {
      pointerDown.current = true;
      mark();
    };
    const onPointerUp = () => {
      if (pointerDown.current) mark();
      pointerDown.current = false;
    };
    el.addEventListener("wheel", onWheel, { passive: true });
    el.addEventListener("touchstart", onTouchStart, { passive: true });
    el.addEventListener("touchmove", onTouchMove, { passive: true });
    el.addEventListener("keydown", onKeyDown);
    el.addEventListener("pointerdown", onPointerDown);
    window.addEventListener("pointerup", onPointerUp);
    window.addEventListener("pointercancel", onPointerUp);
    return () => {
      el.removeEventListener("wheel", onWheel);
      el.removeEventListener("touchstart", onTouchStart);
      el.removeEventListener("touchmove", onTouchMove);
      el.removeEventListener("keydown", onKeyDown);
      el.removeEventListener("pointerdown", onPointerDown);
      window.removeEventListener("pointerup", onPointerUp);
      window.removeEventListener("pointercancel", onPointerUp);
    };
  }, [setPinned]);

  useEffect(() => {
    const follow = () => {
      // Before the first message there is nothing to follow: show the top of the empty state.
      if (!hasContentRef.current) {
        pinned.current = true;
        if (scroller.current) scroller.current.scrollTop = 0;
        lastTop.current = 0;
      } else if (pinned.current) toBottom();
    };
    const ro = new ResizeObserver(follow);
    if (content.current) ro.observe(content.current);
    if (scroller.current) ro.observe(scroller.current);
    follow();
    return () => ro.disconnect();
  }, [toBottom]);

  // The first message arrives: start following it.
  useEffect(() => {
    if (hasContent) jumpToBottom();
  }, [hasContent, jumpToBottom]);

  return { scroller, content, onScroll, atBottom, jumpToBottom };
}
