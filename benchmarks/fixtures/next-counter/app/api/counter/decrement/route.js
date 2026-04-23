import { decrementCount } from "../state";

export async function POST() {
  return Response.json({ count: decrementCount() });
}
