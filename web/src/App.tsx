import { createBrowserRouter, RouterProvider } from "react-router-dom";
import Layout from "./components/Layout";
import Dashboard from "./pages/Dashboard";
import RunBacktest from "./pages/RunBacktest";
import Live from "./pages/Live";
import Explorer from "./pages/Explorer";
import Pools from "./pages/Pools";
import Jobs from "./pages/Jobs";
import Results from "./pages/Results";
import ConfigPage from "./pages/Config";

const router = createBrowserRouter([
  {
    element: <Layout />,
    children: [
      { path: "/", element: <Dashboard /> },
      { path: "/run", element: <RunBacktest /> },
      { path: "/live", element: <Live /> },
      { path: "/explorer", element: <Explorer /> },
      { path: "/pools", element: <Pools /> },
      { path: "/jobs", element: <Jobs /> },
      { path: "/results", element: <Results /> },
      { path: "/results/:runId", element: <Results /> },
      { path: "/config", element: <ConfigPage /> },
    ],
  },
]);

export default function App() {
  return <RouterProvider router={router} />;
}