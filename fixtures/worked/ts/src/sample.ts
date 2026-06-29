// sample.ts — representative TypeScript corpus for habitat-graph PA-1 golden
// Exercises the full TsExtractor taxonomy:
//   B (file), B_Class, B_Class_method, B_Iface, B_fn, B_arrowconst
//   edges: contains, method, inherits (extends + implements), imports_from

import { EventEmitter } from 'events';
import { Logger } from './logger';

// ── Interfaces ────────────────────────────────────────────────────────────────

export interface Walkable {
  walk(distance: number): void;
  getSpeed(): number;
}

export interface Trainable {
  learn(command: string): void;
}

// ── Abstract base class ───────────────────────────────────────────────────────

export abstract class Animal implements Walkable {
  protected name: string;
  private age: number;

  constructor(name: string, age: number) {
    this.name = name;
    this.age = age;
  }

  getName(): string {
    return this.name;
  }

  // Abstract method — emitted as abstract_method_signature by tree-sitter,
  // NOT as method_definition, so it is intentionally SKIPPED by the extractor.
  abstract speak(): void;

  walk(distance: number): void {
    console.log(`${this.name} walks ${distance} meters`);
  }

  getSpeed(): number {
    return 1.0;
  }
}

// ── Concrete subclass ─────────────────────────────────────────────────────────

export class Dog extends Animal implements Walkable, Trainable {
  private breed: string;

  constructor(name: string, age: number, breed: string) {
    super(name, age);
    this.breed = breed;
  }

  speak(): void {
    console.log(`${this.name} says: Woof!`);
  }

  fetch(item: string): void {
    console.log(`${this.name} fetches ${item}`);
  }

  learn(command: string): void {
    console.log(`${this.name} learned: ${command}`);
  }

  getBreed(): string {
    return this.breed;
  }
}

// ── Independent class ─────────────────────────────────────────────────────────

export class Trainer {
  private name: string;

  constructor(name: string) {
    this.name = name;
  }

  train(animal: Animal, command: string): void {
    animal.speak();
  }

  getName(): string {
    return this.name;
  }
}

// ── Standalone exported function ──────────────────────────────────────────────

export function createAnimal(name: string, breed: string): Dog {
  return new Dog(name, 3, breed);
}

// ── Arrow-function const ──────────────────────────────────────────────────────

export const makeTrainer = (name: string): Trainer => {
  return new Trainer(name);
};
