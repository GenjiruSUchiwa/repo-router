/** Loads a transport before anything asks for one. */
declare function preload(name: string): void;

/** A handle the runtime hands back. */
declare class Runtime {
    readonly version: string;
    stop(): void;
}

declare abstract class Bound {
    abstract release(): void;
}

/** What the host promises to pass in. */
declare interface HostEnv {
    readonly cwd: string;
}

declare enum Level {
    Quiet,
    Loud,
}

declare type Handler = (event: string) => void;

declare namespace Runtime {
    const version: string;
}

/** The one entry point this module exports. */
export declare function start(env: HostEnv): Runtime;

export declare class Session {
    private token: string;
    close(): void;
}

export declare const DEFAULT_LEVEL: Level;

export declare namespace Session {
    const active: number;
}
