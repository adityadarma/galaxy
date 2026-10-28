/** ECMAScript and TypeScript global APIs available without an import. */
declare type PropertyKey = string | number | symbol;
declare type Awaited<T> = T extends PromiseLike<infer Value> ? Awaited<Value> : T;
declare type Partial<T> = { [P in keyof T]?: T[P] };
declare type Required<T> = { [P in keyof T]-?: T[P] };
declare type Readonly<T> = { readonly [P in keyof T]: T[P] };
declare type Pick<T, K extends keyof T> = { [P in K]: T[P] };
declare type Record<K extends keyof any, T> = { [P in K]: T };
declare type Exclude<T, U> = T extends U ? never : T;
declare type Extract<T, U> = T extends U ? T : never;
declare type Omit<T, K extends keyof any> = Pick<T, Exclude<keyof T, K>>;
declare type NonNullable<T> = T & {};
declare type Parameters<T extends (...args: any[]) => any> = T extends (...args: infer P) => any ? P : never;
declare type ReturnType<T extends (...args: any[]) => any> = T extends (...args: any[]) => infer R ? R : any;
declare type InstanceType<T extends abstract new (...args: any[]) => any> = T extends abstract new (...args: any[]) => infer R ? R : any;

declare const Object: {
  assign(target: object, ...sources: object[]): object;
  create(prototype: object | null): object;
  entries(value: object): [string, unknown][];
  fromEntries(entries: Iterable<readonly [PropertyKey, unknown]>): object;
  hasOwn(value: object, property: PropertyKey): boolean;
  keys(value: object): string[];
  values(value: object): unknown[];
};

declare const Array: {
  isArray(value: unknown): value is unknown[];
  from<T>(value: Iterable<T> | ArrayLike<T>): T[];
};

declare const String: {
  fromCharCode(...codes: number[]): string;
  raw(template: TemplateStringsArray, ...substitutions: unknown[]): string;
};

interface String {
  at(index: number): string | undefined;
  charAt(index: number): string;
  charCodeAt(index: number): number;
  concat(...strings: string[]): string;
  endsWith(searchString: string, endPosition?: number): boolean;
  indexOf(searchString: string, position?: number): number;
  lastIndexOf(searchString: string, position?: number): number;
  localeCompare(that: string): number;
  match(regexp: string | RegExp): RegExpMatchArray | null;
  padEnd(maxLength: number, fillString?: string): string;
  padStart(maxLength: number, fillString?: string): string;
  repeat(count: number): string;
  replace(searchValue: string | RegExp, replaceValue: string): string;
  replaceAll(searchValue: string | RegExp, replaceValue: string): string;
  search(regexp: string | RegExp): number;
  includes(searchString: string, position?: number): boolean;
  slice(start?: number, end?: number): string;
  split(separator: string | RegExp, limit?: number): string[];
  startsWith(searchString: string, position?: number): boolean;
  substring(start: number, end?: number): string;
  toLowerCase(): string;
  toString(): string;
  toUpperCase(): string;
  trim(): string;
  trimEnd(): string;
  trimStart(): string;
  valueOf(): string;
}

interface Number {
  toExponential(fractionDigits?: number): string;
  toFixed(fractionDigits?: number): string;
  toLocaleString(): string;
  toPrecision(precision?: number): string;
  toString(radix?: number): string;
  valueOf(): number;
}

interface Boolean {
  valueOf(): boolean;
}

interface Array<T> {
  at(index: number): T | undefined;
  concat(...items: (T | T[])[]): T[];
  every(callback: (value: T, index: number) => boolean): boolean;
  find(callback: (value: T, index: number) => boolean): T | undefined;
  findIndex(callback: (value: T, index: number) => boolean): number;
  flat<U>(this: U[][], depth?: number): U[];
  flatMap<U>(callback: (value: T, index: number) => U | U[]): U[];
  forEach(callback: (value: T, index: number) => void): void;
  indexOf(searchElement: T, fromIndex?: number): number;
  map<U>(callback: (value: T, index: number) => U): U[];
  filter(callback: (value: T, index: number) => boolean): T[];
  includes(value: T): boolean;
  join(separator?: string): string;
  pop(): T | undefined;
  push(...items: T[]): number;
  reduce<U>(callback: (previousValue: U, currentValue: T, index: number) => U, initialValue: U): U;
  reverse(): T[];
  slice(start?: number, end?: number): T[];
  some(callback: (value: T, index: number) => boolean): boolean;
  sort(compareFn?: (a: T, b: T) => number): T[];
  splice(start: number, deleteCount?: number, ...items: T[]): T[];
  unshift(...items: T[]): number;
}

declare const Number: {
  isFinite(value: unknown): boolean;
  isInteger(value: unknown): boolean;
  parseFloat(value: string): number;
  parseInt(value: string, radix?: number): number;
};

declare const Boolean: {
  (value?: unknown): boolean;
};

declare const Date: {
  now(): number;
  parse(value: string): number;
  UTC(...values: number[]): number;
};

interface Date {
  getTime(): number;
  toISOString(): string;
  toJSON(): string;
  toLocaleDateString(): string;
  toLocaleString(): string;
  toString(): string;
  valueOf(): number;
}

declare const Math: {
  abs(value: number): number;
  ceil(value: number): number;
  floor(value: number): number;
  max(...values: number[]): number;
  min(...values: number[]): number;
  random(): number;
  round(value: number): number;
};

declare const Promise: {
  all<T>(values: Iterable<T>): Promise<Awaited<T>[]>;
  allSettled<T>(values: Iterable<T>): Promise<PromiseSettledResult<Awaited<T>>[]>;
  race<T>(values: Iterable<T>): Promise<Awaited<T>>;
  reject(reason?: unknown): Promise<never>;
  resolve<T>(value: T): Promise<Awaited<T>>;
};

interface Promise<T> {
  catch<TResult = never>(onrejected?: (reason: unknown) => TResult): Promise<T | TResult>;
  finally(onfinally?: () => void): Promise<T>;
  then<TResult1 = T, TResult2 = never>(onfulfilled?: (value: T) => TResult1, onrejected?: (reason: unknown) => TResult2): Promise<TResult1 | TResult2>;
}

declare class Map<K, V> {
  readonly size: number;
  entries(): Iterable<[K, V]>;
  forEach(callback: (value: V, key: K) => void): void;
  get(key: K): V | undefined;
  set(key: K, value: V): this;
  has(key: K): boolean;
  keys(): Iterable<K>;
  values(): Iterable<V>;
  delete(key: K): boolean;
  clear(): void;
}

declare class Set<T> {
  readonly size: number;
  add(value: T): this;
  entries(): Iterable<[T, T]>;
  forEach(callback: (value: T) => void): void;
  has(value: T): boolean;
  values(): Iterable<T>;
  delete(value: T): boolean;
  clear(): void;
}

declare class WeakMap<K extends object, V> {
  get(key: K): V | undefined;
  set(key: K, value: V): this;
  has(key: K): boolean;
  delete(key: K): boolean;
}

declare class WeakSet<T extends object> {
  add(value: T): this;
  has(value: T): boolean;
  delete(value: T): boolean;
}

declare class Error {
  name: string;
  message: string;
  stack?: string;
}

interface RegExpMatchArray extends Array<string> {
  index?: number;
  input?: string;
}

declare class RegExp {
  test(value: string): boolean;
  exec(value: string): RegExpExecArray | null;
}

declare const JSON: {
  parse(text: string): unknown;
  stringify(value: unknown): string | undefined;
};
