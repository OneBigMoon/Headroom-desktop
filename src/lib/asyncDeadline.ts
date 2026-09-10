/**
 * Bounded waits for backend calls the UI must not stay hostage to.
 *
 * A Tauri command cannot be cancelled once it is in flight, so the honest move
 * when one stops answering is to stop *waiting* on it: the caller keeps its
 * affordance responsive and reports what it knows, while the command itself
 * keeps running and its eventual result can still be adopted.
 */

/**
 * Resolves with the promise's value, or `null` when it has not settled within
 * `ms`. Rejections pass through unchanged, so a call site's error handling
 * still sees the real failure.
 */
export function withDeadline<T>(promise: Promise<T>, ms: number): Promise<T | null> {
  return new Promise<T | null>((resolve, reject) => {
    const timer = setTimeout(() => resolve(null), ms);
    promise.then(
      (value) => {
        clearTimeout(timer);
        resolve(value);
      },
      (error) => {
        clearTimeout(timer);
        reject(error);
      }
    );
  });
}
