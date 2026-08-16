// Import shapes rr extracts from TypeScript. One construct per row of the
// issue's truth table; assertions live in tags.rs unit tests and the golden.
import x from "m-default";
import * as ns from "m-namespace";
import { A } from "m-named";
import { A as B } from "m-aliased";
import mixed, { C } from "m-mixed";
import "m-side-effect";
import ir = require("m-import-equals");
const cj = require("m-require");
const { d } = require("m-destructured");

export { E } from "m-export-named";
export { E as F } from "m-export-aliased";
export * from "m-export-star";
export * as g from "m-export-namespace";

// Not recorded: the argument to a dynamic import is an arbitrary expression,
// and recording only the string-literal case would be a partial list that
// reads as complete.
const dynamic = import("./dynamic");