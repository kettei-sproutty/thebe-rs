import { getCount } from "../../utils/counter";

export default defineEventHandler(() => ({
  count: getCount(),
}));
