import { resetCount } from "../../utils/counter";

export default defineEventHandler(() => ({
  count: resetCount(),
}));
