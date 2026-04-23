let count = 0;

export function getCount() {
  return count;
}

export function incrementCount() {
  count += 1;
  return count;
}

export function decrementCount() {
  count -= 1;
  return count;
}

export function resetCount() {
  count = 0;
  return count;
}
