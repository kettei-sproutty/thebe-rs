import { incrementCount } from "../../utils/counter";

export default defineEventHandler(() => ({
  count: incrementCount(),
}));
