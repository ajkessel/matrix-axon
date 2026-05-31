-- Automatically maintain updated_at on every UPDATE to the accounts table.
-- The application no longer needs to include `updated_at = now()` in UPDATE
-- queries; the trigger fires before each row update and sets it implicitly.

CREATE OR REPLACE FUNCTION trigger_set_updated_at()
RETURNS TRIGGER AS $$
BEGIN
  NEW.updated_at = now();
  RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER accounts_set_updated_at
  BEFORE UPDATE ON accounts
  FOR EACH ROW EXECUTE FUNCTION trigger_set_updated_at();
