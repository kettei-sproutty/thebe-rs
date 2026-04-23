import CounterClient from "./CounterClient";
import { getCount } from "./api/counter/state";

export default function Page() {
  return <CounterClient initialCount={getCount()} title="Next Counter" />;
}
