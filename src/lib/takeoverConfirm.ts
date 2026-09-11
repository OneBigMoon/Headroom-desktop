import { useCallback, useEffect, useRef, useState } from "react";

/// The question a connector asks before it replaces the tool that currently
/// owns a client's route.
export interface TakeoverPrompt {
  /// The client's display name, for the dialog title.
  label: string;
  /// The tool that is routing right now, named in the body.
  provider: string;
}

export interface TakeoverConfirm {
  prompt: TakeoverPrompt | null;
  /// Resolves true only when the user accepts. Both callers -- the settings
  /// switch and the launcher's auto-configure loop -- await this instead of
  /// `window.confirm`.
  request: (label: string, provider: string) => Promise<boolean>;
  answer: (confirmed: boolean) => void;
}

/// Ask before taking over another tool's route.
///
/// This must be an in-app dialog rather than a native `window.confirm`: this is
/// a WKWebView, and WebKit answers `window.confirm` with "cancel" whenever the
/// UI delegate does not implement the panel -- wry's does not. The native call
/// therefore returned false without drawing anything, and every attempt to
/// enable the Codex connector did nothing at all: no dialog, no error, no
/// state change.
export function useTakeoverConfirm(): TakeoverConfirm {
  const [prompt, setPrompt] = useState<TakeoverPrompt | null>(null);
  const answerRef = useRef<((confirmed: boolean) => void) | null>(null);

  const request = useCallback((label: string, provider: string) => {
    return new Promise<boolean>((resolve) => {
      // A second prompt can only arrive before the first was answered; release
      // that caller as a decline instead of leaving it waiting forever.
      answerRef.current?.(false);
      answerRef.current = resolve;
      setPrompt({ label, provider });
    });
  }, []);

  const answer = useCallback((confirmed: boolean) => {
    const resolve = answerRef.current;
    answerRef.current = null;
    setPrompt(null);
    resolve?.(confirmed);
  }, []);

  // Escape is the keyboard equivalent of the backdrop click, so the prompt
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
