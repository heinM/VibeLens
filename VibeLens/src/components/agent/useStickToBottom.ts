import { useCallback, useEffect, useRef, useState } from "react";

/**
 * Keeps a scroller pinned to the bottom while content grows, but only if the user was
 * already at the bottom. Scrolling up releases the pin; `jumpToBottom` re-engages it.
 */
export function useStickToBottom(hasContent = true) {
  const scroller = useRef<HTMLDivElement>(null);
  const content = useRef<HTMLDivElement>(null);
  const pinned = useRef(true);
  const hasContentRef = useRef(hasContent);
  hasContentRef.current = hasContent;
  const [atBottom, setAtBottom] = useState(true);

  const toBottom = useCallback(() => {
    const el = scroller.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, []);

  const onScroll = useCallback(() => {
    const el = scroller.current;
    if (!el) return;
    const near = el.scrollHeight - el.scrollTop - el.clientHeight < 32;
    pinned.current = near;
    setAtBottom(near);
  }, []);

  const jumpToBottom = useCallback(() => {
    pinned.current = true;
    setAtBottom(true);
    toBottom();
  }, [toBottom]);

  useEffect(() => {
    const follow = () => {
      // Before the first message there is nothing to follow: show the top of the empty state.
      if (!hasContentRef.current) {
        pinned.current = true;
        if (scroller.current) scroller.current.scrollTop = 0;
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
    if (hasContent) {
      pinned.current = true;
      toBottom();
    }
  }, [hasContent, toBottom]);

  return { scroller, content, onScroll, atBottom, jumpToBottom };
}
