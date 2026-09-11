import { useCallback, useEffect, useRef, useState } from "react";

/// The question an in-app confirm dialog shows. Every string arrives already
/// translated, so the dialog itself stays presentation only.
export interface ConfirmPrompt {
  title: string;
  body: string;
  confirmLabel: string;
  cancelLabel: string;
}

export interface ConfirmDialogController {
  prompt: ConfirmPrompt | null;
  /// Resolves true only when the user accepts.
  request: (prompt: ConfirmPrompt) => Promise<boolean>;
  answer: (confirmed: boolean) => void;
}

/// Ask a yes/no question in the app window.
///
/// This must be an in-app dialog rather than a native `window.confirm`: this is
/// a WKWebView, and WebKit answers `window.confirm` with "cancel" whenever the
/// UI delegate does not implement the panel -- wry's does not. The native call
/// therefore returned false without drawing anything, so every action behind it
/// silently did nothing: no dialog, no error, no state change.
export function useConfirmDialog(): ConfirmDialogController {
  const [prompt, setPrompt] = useState<ConfirmPrompt | null>(null);
  const answerRef = useRef<((confirmed: boolean) => void) | null>(null);

  const request = useCallback((next: ConfirmPrompt) => {
    return new Promise<boolean>((resolve) => {
      // A second question can only arrive before the first was answered;
      // release that caller as a decline instead of leaving it waiting forever.
      answerRef.current?.(false);
      answerRef.current = resolve;
      setPrompt(next);
    });
  }, []);

  const answer = useCallback((confirmed: boolean) => {
    const resolve = answerRef.current;
    answerRef.current = null;
    setPrompt(null);
    resolve?.(confirmed);
  }, []);

  // Escape is the keyboard equivalent of the backdrop click, so the question
  // never traps someone who changed their mind.
  useEffect(() => {
    if (!prompt) return;
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") answer(false);
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [prompt, answer]);

  return { prompt, request, answer };
}
