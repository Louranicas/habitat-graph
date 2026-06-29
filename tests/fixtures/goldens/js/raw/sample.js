// JavaScript golden corpus for habitat-graph parity gate.
// Covers: ES6 imports, exported/non-exported classes, local inheritance,
// external inheritance (extends EventEmitter), instance/static/private methods,
// arrow functions, function expressions, and standalone function declarations.

import EventEmitter from 'events';
import { readFile, writeFile } from 'fs/promises';
import path from 'path';

// ── Standalone top-level function declarations ─────────────────────────────

function createId(prefix) {
    return `${prefix}_${Date.now()}`;
}

function validateName(name) {
    return typeof name === 'string' && name.length > 0;
}

// ── Arrow function and function-expression constants ───────────────────────

const formatDate = (date) => date.toISOString();

const parseQuery = function(raw) {
    return raw.trim();
};

// ── Base class (no heritage) ───────────────────────────────────────────────

class Animal {
    constructor(name, type) {
        this.name = name;
        this.type = type;
    }

    speak() {
        return `${this.name} makes a noise.`;
    }

    toString() {
        return `[${this.type}] ${this.name}`;
    }

    static create(name, type) {
        return new Animal(name, type);
    }
}

// ── Local subclass (extends local Animal) ─────────────────────────────────

class Dog extends Animal {
    constructor(name) {
        super(name, 'dog');
    }

    speak() {
        return `${this.name} barks.`;
    }

    fetch(item) {
        return `${this.name} fetches ${item}`;
    }

    #validate() {
        return this.name.length > 0;
    }
}

// ── Class extending an external base (EventEmitter not defined in this file)

class EventBus extends EventEmitter {
    constructor() {
        super();
        this.channels = new Map();
    }

    subscribe(event, handler) {
        this.on(event, handler);
        return this;
    }

    publish(event, data) {
        this.emit(event, data);
        return this;
    }

    unsubscribe(event) {
        this.removeAllListeners(event);
    }
}

export { Animal, Dog, EventBus, createId, validateName, formatDate, parseQuery };
