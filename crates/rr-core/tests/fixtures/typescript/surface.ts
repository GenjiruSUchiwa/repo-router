/** Options the caller controls. */
export interface ClientOptions {
    /** Base URL every request is joined onto. */
    readonly baseUrl: string;
    retries?: number;
    describe(): string;
}

/** How a transport names itself. */
export type TransportName = "http" | "memory";

/** Retry ceilings, by transport. */
export enum Ceiling {
    Http = 3,
    Memory = 0,
}

/** The ceiling used when nothing says otherwise. */
export const DEFAULT_CEILING = 3;

// Prefix every internal key carries.
// A run of two lines is one document.
const INTERNAL_PREFIX = "rr";

/** Builds a client bound to one transport. */
export const makeClient = (options: ClientOptions): Client => new Client(options);

const later =
    (options: ClientOptions) => new Client(options);

/** A configured client. */
@sealed
export class Client implements ClientOptions {
    readonly baseUrl: string;
    #attempts = 0;
    private secret: string;
    protected shared: number;
    static registry = new Map<string, Client>();

    /** Binds the client to its options. */
    constructor(options: ClientOptions) {
        this.baseUrl = options.baseUrl;
        this.secret = INTERNAL_PREFIX;
        this.shared = 0;
    }

    /** How many attempts this client has made. */
    get attempts(): number {
        return this.#attempts;
    }

    set attempts(value: number) {
        this.#attempts = value;
    }

    describe(): string {
        return this.baseUrl;
    }

    /** Sends one request. */
    async send(path: string): Promise<string> {
        const target = join(this.baseUrl, path);
        return await fetchOnce(target);
    }

    private reset(): void {
        this.#attempts = 0;
    }
}

/** What every transport has to provide. */
export abstract class Transport {
    abstract deliver(body: string): Promise<void>;
}

export declare function configure(options: ClientOptions): void;

/** Yields one page number per page. */
export function* pages(count: number): Generator<number> {
    yield count;
}

/** Names reserved by the router. */
export namespace Reserved {
    export const NAMES = ["rr"];

    /** Memoized lookups. */
    const CACHE = new Set<string>();

    export function includes(name: string): boolean {
        return CACHE.has(name) || NAMES.includes(name);
    }
}

/** Joins a base and a path. */
function join(base: string, path: string): string {
    "use strict";
    return `${base}/${path}`;
}
