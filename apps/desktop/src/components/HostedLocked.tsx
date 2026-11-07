import { Lock } from "lucide-react";
import { useNavigate } from "react-router-dom";

import { useI18n } from "../lib/i18n";
import { Button } from "./ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "./ui/card";

/** 官方托管实例只读：交易、账户与策略相关页面统一在这里拦下。 */
export function HostedLocked() {
  const { t } = useI18n();
  const navigate = useNavigate();

  return (
    <Card>
      <CardHeader>
        <div>
          <CardTitle>{t.hosted.lockedTitle}</CardTitle>
          <CardDescription>{t.hosted.lockedBody}</CardDescription>
        </div>
        <Lock size={16} className="text-subtle" />
      </CardHeader>
      <CardContent>
        <Button size="sm" onClick={() => navigate("/settings?tab=connection")}>
          {t.hosted.goSettings}
        </Button>
      </CardContent>
    </Card>
  );
}
