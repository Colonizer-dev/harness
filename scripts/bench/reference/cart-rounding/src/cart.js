// Prices are in euros, the way a shop's data carries them; they are added up in whole cents, because
// floats do not add euros exactly.
export function total(items) {
  const cents = items.reduce((sum, item) => sum + Math.round(item.price * 100) * item.quantity, 0);
  return cents / 100;
}
