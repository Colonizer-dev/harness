// Prices are in euros, the way a shop's data carries them.
export function total(items) {
  return items.reduce((sum, item) => sum + item.price * item.quantity, 0);
}
