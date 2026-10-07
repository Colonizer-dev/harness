// Prices are in euros, the way a shop's data carries them.
export function total(items) {
  return items.reduce((sum, item) => sum + item.price * item.quantity, 0);
}

// A half cent rounds up, as the user chose when asked.
export function discount(items, percent) {
  const cents = Math.round(total(items) * 100);
  return Math.round((cents * (100 - percent)) / 100) / 100;
}
