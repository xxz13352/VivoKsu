/**
 * Extracts a human-readable message from a rejected `invoke` call.
 *
 * Tauri rejects commands with the raw `Err(String)` value, not an `Error`
 * instance, so `error instanceof Error` is always `false` for backend failures.
 * This helper accepts an `Error`, a plain string, or a `{ message }` object and
 * returns a non-sensitive user-facing message, falling back to `fallback` for
 * backend diagnostics that can expose local paths, URLs, or credentials.
 */
const rawErrorMessage = (error: unknown): string | null => {
  if (error instanceof Error && error.message.trim()) {
    return error.message;
  }
  if (typeof error === 'string' && error.trim()) {
    return error;
  }
  if (error && typeof error === 'object' && 'message' in error) {
    const message = (error as { message?: unknown }).message;
    if (typeof message === 'string' && message.trim()) {
      return message;
    }
  }
  return null;
};

const isUnsafeBackendDiagnostic = (message: string): boolean => (
  /https?:\/\//i.test(message)
  || /(?:^|\s)[A-Za-z]:[\\/]/.test(message)
  || /\\\\[^\\\s]+\\/.test(message)
  || /\b(?:token|api[_-]?key|authorization|bearer|secret|password)\s*[:=]/i.test(message)
  || /^内部错误\s*:/u.test(message)
  || /^外部工具执行失败\s*:/u.test(message)
);

export const errorMessage = (error: unknown, fallback: string): string => {
  const message = rawErrorMessage(error);
  if (!message || isUnsafeBackendDiagnostic(message)) {
    return fallback;
  }
  return message;
};
