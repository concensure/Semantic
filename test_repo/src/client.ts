export function retryRequest(attempts: number): Promise<number> {
  if (attempts <= 0) {
    throw new Error('attempts must be > 0');
  }
  return fetchWithRetry(attempts);
}

function fetchWithRetry(count: number): Promise<number> {
  return Promise.resolve(count);
}

export class ApiClient {
  constructor(private readonly baseUrl: string) {}

  async getStatus(): Promise<string> {
    const result = await retryRequest(3);
    return `${this.baseUrl}:${result}`;
  }
}

export async function fetchData(token?: string): Promise<number> {
  if (!token) {
    throw new Error("missing");
  }

  await retryRequest(1);
  return retryRequest(2);
}
