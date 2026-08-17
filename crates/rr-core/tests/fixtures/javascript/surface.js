import { helper } from './helper.js';

/** Documented class. */
export class Service extends Base {
  #secret = 1;
  constructor(name) {
    super();
    this.name = name;
  }
  run() {
    return helper(this.name);
  }
  static build() {
    return new Service('x');
  }
  get size() {
    return 1;
  }
}

export function bare(a) {
  return a;
}

export const arrow = (a) => a;
const value = 3;
export default Service;
