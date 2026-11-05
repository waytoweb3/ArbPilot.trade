import Hero from "../components/Hero";
import Pricing from "../components/Pricing";
import {
  DownloadSection,
  FundingSection,
  NewsSection,
  StrategySection,
  VenuesStrip,
} from "../components/Sections";

export default function HomePage() {
  return (
    <>
      <Hero />
      <VenuesStrip />
      <FundingSection />
      <NewsSection />
      <StrategySection />
      <Pricing />
      <DownloadSection />
    </>
  );
}
