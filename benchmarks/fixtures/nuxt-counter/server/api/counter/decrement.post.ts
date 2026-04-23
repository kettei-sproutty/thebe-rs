import { decrementCount } from "../../utils/counter";

export default defineEventHandler(() => ({
  count: decrementCount(),
}));
