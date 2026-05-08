import { useEffect, useRef } from 'react';

/// Closes a popover when the user taps/clicks outside the wrapper, or
/// presses Escape. Touch devices need this because there's no `mouseleave`
/// to clear hover state.
///
/// `close` is held in a ref so callers can pass a fresh inline arrow each
/// render without retriggering the effect — otherwise document listeners
/// would be torn down and reattached on every parent re-render while open.
export function useDismissOnOutside(open: boolean, close: () => void) {
  const ref = useRef<HTMLElement | null>(null);
  const closeRef = useRef(close);
  useEffect(() => {
    closeRef.current = close;
  });

  useEffect(() => {
    if (!open) return;
    const onPointer = (e: PointerEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) closeRef.current();
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') closeRef.current();
    };
    document.addEventListener('pointerdown', onPointer);
    document.addEventListener('keydown', onKey);
    return () => {
      document.removeEventListener('pointerdown', onPointer);
      document.removeEventListener('keydown', onKey);
    };
  }, [open]);
  return ref;
}
