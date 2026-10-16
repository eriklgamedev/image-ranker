// Local, fail-closed validator for the draft-07 keywords used by our merged
// Tauri configuration. This is not a general-purpose JSON Schema implementation.
import {isDeepStrictEqual} from 'node:util';

const annotations = new Set(['$schema', '$id', '$comment', 'title', 'description',
  'default', 'examples', 'deprecated', 'readOnly', 'writeOnly', 'definitions']);
const supported = new Set(['$ref', 'type', 'enum', 'const', 'allOf', 'anyOf', 'oneOf',
  'not', 'if', 'then', 'else', 'properties', 'patternProperties', 'additionalProperties',
  'required', 'minProperties', 'maxProperties', 'propertyNames', 'dependencies',
  'items', 'additionalItems', 'minItems', 'maxItems', 'uniqueItems', 'contains',
  'minLength', 'maxLength', 'pattern', 'format', 'minimum', 'maximum',
  'exclusiveMinimum', 'exclusiveMaximum', 'multipleOf']);

export class UnsupportedSchema extends Error {}
const object = value => value !== null && typeof value === 'object' && !Array.isArray(value);
const typed = (value, type) => type === 'object' ? object(value)
  : type === 'array' ? Array.isArray(value)
  : type === 'null' ? value === null
  : type === 'integer' ? Number.isInteger(value)
  : type === 'number' ? typeof value === 'number' && Number.isFinite(value)
  : typeof value === type;

export function validateSchema(value, schema, root = schema, path = '$', depth = 0) {
  if (depth > 128) throw new UnsupportedSchema('Schema recursion limit');
  if (schema === true) return [];
  if (schema === false) return [`${path}: forbidden by schema`];
  if (!object(schema)) throw new UnsupportedSchema('Malformed schema');
  for (const key of Object.keys(schema)) {
    if (!supported.has(key) && !annotations.has(key)) throw new UnsupportedSchema(`Unsupported schema keyword ${key} at ${path}`);
  }
  const check = (item, rule, location = path) => validateSchema(item, rule, root, location, depth + 1);
  if (schema.$ref) {
    if (!schema.$ref.startsWith('#/')) throw new UnsupportedSchema('Remote schema references are disabled');
    const resolved = schema.$ref.slice(2).split('/').reduce((node, key) => node?.[key.replaceAll('~1', '/').replaceAll('~0', '~')], root);
    if (resolved === undefined) throw new UnsupportedSchema(`Missing local reference ${schema.$ref}`);
    return check(value, resolved); // draft-07: ref siblings do not apply.
  }
  const errors = [];
  const fail = text => errors.push(`${path}: ${text}`);
  if (schema.type && ![].concat(schema.type).some(type => typed(value, type))) fail(`expected ${JSON.stringify(schema.type)}`);
  if (schema.enum && !schema.enum.some(item => isDeepStrictEqual(item, value))) fail('not in enum');
  if ('const' in schema && !isDeepStrictEqual(schema.const, value)) fail('const mismatch');
  for (const child of schema.allOf ?? []) errors.push(...check(value, child));
  for (const keyword of ['anyOf', 'oneOf']) if (schema[keyword]) {
    // Evaluate every alternative, including unsupported keywords: never hide an
    // incomplete validator behind an apparently passing alternative.
    const matches = schema[keyword].map(child => check(value, child)).filter(items => !items.length).length;
    if (keyword === 'anyOf' ? matches === 0 : matches !== 1) fail(`${keyword} did not match`);
  }
  if (schema.not && !check(value, schema.not).length) fail('matches forbidden schema');
  if (schema.if) {
    const branch = check(value, schema.if).length ? schema.else : schema.then;
    if (branch !== undefined) errors.push(...check(value, branch));
  }
  if (object(value)) {
    const keys = Object.keys(value);
    for (const key of schema.required ?? []) if (!Object.hasOwn(value, key)) fail(`missing ${key}`);
    if (schema.minProperties !== undefined && keys.length < schema.minProperties) fail('too few properties');
    if (schema.maxProperties !== undefined && keys.length > schema.maxProperties) fail('too many properties');
    for (const key of keys) {
      if (schema.propertyNames) errors.push(...check(key, schema.propertyNames, `${path}.${key}`));
      const matching = Object.entries(schema.patternProperties ?? {}).filter(([pattern]) => new RegExp(pattern).test(key));
      if (Object.hasOwn(schema.properties ?? {}, key)) matching.push(['property', schema.properties[key]]);
      if (!matching.length && schema.additionalProperties !== undefined) matching.push(['additional', schema.additionalProperties]);
      for (const [, child] of matching) errors.push(...check(value[key], child, `${path}.${key}`));
    }
    for (const [key, rule] of Object.entries(schema.dependencies ?? {})) if (Object.hasOwn(value, key)) {
      if (Array.isArray(rule)) {
        for (const dependency of rule) if (!Object.hasOwn(value, dependency)) fail(`${key} requires ${dependency}`);
      } else errors.push(...check(value, rule));
    }
  }
  if (Array.isArray(value)) {
    if (schema.minItems !== undefined && value.length < schema.minItems) fail('too few items');
    if (schema.maxItems !== undefined && value.length > schema.maxItems) fail('too many items');
    if (schema.uniqueItems && value.some((item, index) => value.slice(0, index).some(other => isDeepStrictEqual(item, other)))) fail('duplicate items');
    value.forEach((item, index) => {
      const rule = Array.isArray(schema.items) ? (schema.items[index] ?? schema.additionalItems) : schema.items;
      if (rule !== undefined) errors.push(...check(item, rule, `${path}[${index}]`));
    });
    if (schema.contains && !value.some(item => !check(item, schema.contains).length)) fail('contains did not match');
  }
  if (typeof value === 'string') {
    const length = [...value].length;
    if (schema.minLength !== undefined && length < schema.minLength) fail('string too short');
    if (schema.maxLength !== undefined && length > schema.maxLength) fail('string too long');
    if (schema.pattern && !new RegExp(schema.pattern).test(value)) fail('pattern mismatch');
    if (schema.format) {
      if (!['uri', 'uri-reference', 'uuid'].includes(schema.format)) throw new UnsupportedSchema(`Unsupported format ${schema.format}`);
      if (schema.format === 'uuid') {
        if (!/^[0-9a-f]{8}(-[0-9a-f]{4}){3}-[0-9a-f]{12}$/i.test(value)) fail('invalid UUID');
      } else {
        try { new URL(value, schema.format === 'uri-reference' ? 'https://schema.invalid/' : undefined); }
        catch { fail(`invalid ${schema.format}`); }
      }
    }
  }
  if (typeof value === 'number') {
    for (const [key, invalid] of [['minimum', value < schema.minimum], ['maximum', value > schema.maximum],
      ['exclusiveMinimum', value <= schema.exclusiveMinimum], ['exclusiveMaximum', value >= schema.exclusiveMaximum]]) {
      if (schema[key] !== undefined && invalid) fail(key);
    }
    if (schema.multipleOf !== undefined && !Number.isInteger(value / schema.multipleOf)) fail('multipleOf');
  }
  return errors;
}

export function mergePatch(target, patch) {
  if (!object(patch)) return structuredClone(patch);
  const result = object(target) ? structuredClone(target) : {};
  for (const [key, value] of Object.entries(patch)) {
    if (['__proto__', 'constructor', 'prototype'].includes(key)) throw new Error('Unsafe configuration key');
    if (value === null) delete result[key];
    else result[key] = mergePatch(result[key], value);
  }
  return result;
}
