export class BridgeError extends Error {
  constructor(public readonly code: string, message: string, options?: ErrorOptions) {
    super(message, options);
    this.name = 'BridgeError';
  }
}

export function errorCode(error: unknown): string {
  if (error instanceof BridgeError) return error.code;
  if (error && typeof error === 'object' && 'code' in error && typeof error.code === 'string') {
    return error.code;
  }
  return 'UNEXPECTED_ERROR';
}
