export function greet(name) {
  return `Hello, ${name}!`;
}

export function shout(name) {
  return greet(name).toUpperCase();
}
